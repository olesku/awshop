//! EKS discovery and kubeconfig operations. No terminal or preference-store code.
use awshop_plugin::{
  account, json, output, Account, Context, Dependency, Field, FieldKind, Launch, Outcome, Plugin, Resource, Result,
  SessionSpec, Settings, Setup,
};
use serde_json::{json as value, Value};
use std::{
  env,
  ffi::OsString,
  path::{Path, PathBuf},
  process::Command,
};

/// Lists EKS clusters and writes kubeconfig entries with `aws eks update-kubeconfig`.
pub struct Eks;

impl Plugin for Eks {
  fn name(&self) -> &'static str {
    "eks"
  }

  fn title(&self) -> &'static str {
    "EKS"
  }

  fn description(&self) -> &'static str {
    "Select an EKS cluster and configure kubectl"
  }

  fn dependencies(&self) -> &'static [Dependency] {
    &[
      Dependency {
        name: "aws",
        installation: "Install AWS CLI v2 and add it to PATH.",
        optional: false,
      },
      Dependency {
        name: "kubectl",
        installation: "Install kubectl and add it to PATH.",
        optional: false,
      },
      Dependency {
        name: "k9s",
        installation: "Install k9s to open clusters in it.",
        optional: true,
      },
    ]
  }

  fn session(&self) -> Option<SessionSpec> {
    Some(SessionSpec {
      file_field: "kubeconfig",
      environment: "KUBECONFIG",
      scope_label: "Kubeconfig scope",
      action: "Start EKS session",
    })
  }

  fn discover(&self, context: &Context) -> Result<Vec<Resource>> {
    let Account { id: account, partition } = account(context)?;
    let clusters = json(&mut context.aws("eks", "list-clusters")?, "EKS cluster discovery")?;
    let names = clusters["clusters"]
      .as_array()
      .ok_or("EKS response did not contain a cluster list.")?;

    let mut resources = Vec::new();
    for name in names {
      let name = name.as_str().ok_or("EKS returned an invalid cluster name.")?;
      resources.push(Resource {
        id: format!("arn:{partition}:eks:{}:{account}:cluster/{name}", context.region),
        label: name.to_owned(),
        columns: vec![String::new(); 2],
        current: false,
      });
    }

    resources.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(resources)
  }

  /// The cluster of kubectl's current context, from every KUBECONFIG file, including a
  /// session's.
  fn mark_current(&self, context: &Context, resources: &mut [Resource]) -> Result<()> {
    let view = kubeconfig(context, None)?;
    let current = contexts(&view).find(|entry| entry["name"] == view["current-context"]);
    for resource in resources {
      resource.current = current.is_some_and(|entry| entry["context"]["cluster"] == resource.id);
    }
    Ok(())
  }

  fn headings(&self) -> &'static [&'static str] {
    &["Cluster", "Version", "Status"]
  }

  /// The Kubernetes version and status from `eks describe-cluster`. Without
  /// eks:DescribeCluster the list shows neither.
  fn columns(&self, context: &Context, resource: &Resource) -> Option<Vec<String>> {
    let mut command = context.aws("eks", "describe-cluster").ok()?;
    command.args(["--name", &resource.label]);
    let described = json(&mut command, "EKS cluster description").ok()?;
    let cluster = &described["cluster"];
    let version = format!("v{}", cluster["version"].as_str()?);
    let status = cluster["status"].as_str().unwrap_or_default().to_owned();
    Some(vec![version, status])
  }

  /// Prefill from saved settings, else from an existing context for this cluster and
  /// profile, else defaults.
  fn prepare(&self, context: &Context, resource: &Resource, saved: &Settings) -> Result<Setup> {
    let path = saved
      .get("kubeconfig")
      .map(PathBuf::from)
      .unwrap_or_else(|| kubeconfig_path(context.original_env("KUBECONFIG")));
    let view = kubeconfig(context, Some(&path))?;
    let existing = contexts(&view).find(|entry| {
      entry["context"]["cluster"] == resource.id && user_profile(&view, entry) == Some(context.profile.as_str())
    });
    let alias = existing
      .and_then(|entry| entry["name"].as_str())
      .map(str::to_owned)
      .unwrap_or_else(|| format!("{}/{}/{}", context.profile, context.region, resource.label));
    let role = existing.and_then(|entry| user_role(&view, entry)).unwrap_or_default();
    let configured = contexts(&view).any(|entry| entry["context"]["cluster"] == resource.id);

    let mut fields = vec![
      Field {
        key: "role_arn",
        label: "Authentication",
        description: "Choose which IAM role kubectl uses to authenticate to the cluster.",
        kind: FieldKind::SavedChoice {
          collection: "iam-roles",
          default_label: "Use AWS profile",
          prompt: "Role ARN for kubectl authentication",
        },
        value: role,
      },
      Field {
        key: "alias",
        label: "Context name",
        description: "Choose the name shown by kubectl for this cluster and authentication setup.",
        kind: FieldKind::Text,
        value: alias,
      },
      Field {
        key: "kubeconfig",
        label: "Kubeconfig",
        description: "Choose the file to update. Its current context will be set to this cluster.",
        kind: FieldKind::FilePath,
        value: path.to_string_lossy().into_owned(),
      },
      Field {
        key: "assume_role_arn",
        label: "Lookup role",
        description:
          "Choose a role for retrieving cluster information from AWS. It does not change kubectl authentication.",
        kind: FieldKind::SavedChoice {
          collection: "iam-roles",
          default_label: "Use AWS profile",
          prompt: "Role ARN for retrieving cluster information",
        },
        value: String::new(),
      },
    ];

    for field in &mut fields {
      if let Some(value) = saved.get(field.key) {
        field.value.clone_from(value);
      }
    }

    let verb = if configured { "Update" } else { "Configure" };
    Ok(Setup {
      action: format!("{verb} and activate"),
      launch: context.executables.contains_key("k9s").then(|| Launch {
        action: format!("{verb} and open k9s"),
        session_action: "Start EKS session and open k9s".to_owned(),
        description:
          "Do the same, then open k9s on this cluster. Quitting k9s returns you to the shell with the setup in place."
            .to_owned(),
      }),
      fields,
      notes: vec!["Sets the current context in the selected kubeconfig file.".to_owned()],
    })
  }

  fn validate_field(&self, field: &str, value: &str) -> Result<()> {
    if value.chars().any(char::is_control) {
      return Err("Control characters are not allowed.".into());
    }

    match field {
      "role_arn" | "assume_role_arn" if !value.is_empty() => validate_role(value),
      "alias" | "kubeconfig" if value.is_empty() => Err("This value cannot be empty.".into()),
      "alias" | "kubeconfig" | "role_arn" | "assume_role_arn" => Ok(()),
      _ => Err(format!("Unknown EKS setting '{field}'.").into()),
    }
  }

  fn execute(&self, context: &Context, resource: &Resource, settings: &Settings) -> Result<Outcome> {
    let path = PathBuf::from(required(settings, "kubeconfig")?);
    let alias = required(settings, "alias")?;
    let view = kubeconfig(context, Some(&path))?;
    validate_alias(&view, alias, &resource.id, &context.profile)?;

    output(
      &mut update_command(context, resource, settings)?,
      "EKS kubeconfig update",
    )?;

    Ok(Outcome {
      message: format!("Activated Kubernetes context {alias}"),
      details: vec![format!("Kubeconfig: {}", path.display())],
      open: None,
    })
  }

  /// k9s on the context that `execute` just wrote.
  fn launch(&self, context: &Context, _resource: &Resource, settings: &Settings) -> Result<Command> {
    let mut command = context.command("k9s")?;
    command
      .env("KUBECONFIG", required(settings, "kubeconfig")?)
      .args(["--context", required(settings, "alias")?]);
    Ok(command)
  }
}

/// `aws eks update-kubeconfig`. The context and user share the alias, so contexts
/// for different profiles keep separate authentication.
fn update_command(context: &Context, resource: &Resource, settings: &Settings) -> Result<Command> {
  let mut command = context.aws("eks", "update-kubeconfig")?;
  let alias = required(settings, "alias")?;
  command.args([
    "--name",
    &resource.label,
    "--alias",
    alias,
    "--user-alias",
    alias,
    "--kubeconfig",
    required(settings, "kubeconfig")?,
  ]);

  for (key, flag) in [("role_arn", "--role-arn"), ("assume_role_arn", "--assume-role-arn")] {
    if let Some(value) = settings.get(key).filter(|value| !value.is_empty()) {
      command.args([flag, value]);
    }
  }

  Ok(command)
}

/// A setting that must have a value.
fn required<'a>(settings: &'a Settings, key: &str) -> Result<&'a str> {
  settings
    .get(key)
    .map(String::as_str)
    .filter(|value| !value.is_empty())
    .ok_or_else(|| format!("Missing EKS setting '{key}'.").into())
}

/// The first non-empty path in a KUBECONFIG value, or `~/.kube/config`.
fn kubeconfig_path(paths: Option<OsString>) -> PathBuf {
  paths
    .and_then(|paths| env::split_paths(&paths).find(|path| !path.as_os_str().is_empty()))
    .unwrap_or_else(|| awshop_plugin::home().join(".kube/config"))
}

/// One kubeconfig file as JSON, empty when it does not exist yet. Without a path, every
/// file in KUBECONFIG merged, as kubectl sees them.
fn kubeconfig(context: &Context, path: Option<&Path>) -> Result<Value> {
  // config view only reads local configuration; it never invokes authentication.
  let mut command = context.command("kubectl")?;
  command.args(["config", "view", "--output", "json"]);

  if let Some(path) = path {
    if !path.try_exists()? {
      return Ok(value!({"contexts": [], "users": []}));
    }
    command.arg("--kubeconfig").arg(path);
  }

  json(&mut command, "Reading kubeconfig")
}

/// The contexts in a kubeconfig view.
fn contexts(view: &Value) -> impl Iterator<Item = &Value> {
  view["contexts"].as_array().into_iter().flatten()
}

/// The kubeconfig user referenced by a context entry.
fn user<'a>(view: &'a Value, entry: &Value) -> Option<&'a Value> {
  view["users"]
    .as_array()?
    .iter()
    .find(|user| user["name"] == entry["context"]["user"])
}

/// The AWS_PROFILE that a context's exec credential plugin uses.
fn user_profile<'a>(view: &'a Value, entry: &Value) -> Option<&'a str> {
  user(view, entry)?["user"]["exec"]["env"]
    .as_array()?
    .iter()
    .find(|variable| variable["name"] == "AWS_PROFILE")?["value"]
    .as_str()
}

/// The role ARN passed to `aws eks get-token`, if any.
fn user_role(view: &Value, entry: &Value) -> Option<String> {
  let args = user(view, entry)?["user"]["exec"]["args"].as_array()?;
  args
    .windows(2)
    .find(|pair| pair[0] == "--role" || pair[0] == "--role-arn")?[1]
    .as_str()
    .map(str::to_owned)
}

/// Refuse an alias that already names a context or user for another cluster or AWS
/// profile, since update-kubeconfig would overwrite its authentication.
fn validate_alias(view: &Value, alias: &str, cluster_arn: &str, profile: &str) -> Result<()> {
  for entry in contexts(view) {
    if (entry["name"] == alias || entry["context"]["user"] == alias)
      && (entry["context"]["cluster"] != cluster_arn || user_profile(view, entry) != Some(profile))
    {
      return Err(
        format!(
          "Context or user name '{alias}' is already used by another cluster or AWS profile. Choose another context name."
        )
        .into(),
      );
    }
  }

  Ok(())
}

/// Accept `arn:<partition>:iam::<12 digits>:role/<name>`.
fn validate_role(arn: &str) -> Result<()> {
  let parts: Vec<_> = arn.splitn(6, ':').collect();
  if parts.len() != 6
    || parts[0] != "arn"
    || parts[1].is_empty()
    || parts[2] != "iam"
    || !parts[3].is_empty()
    || parts[4].len() != 12
    || !parts[4].bytes().all(|byte| byte.is_ascii_digit())
    || !parts[5].starts_with("role/")
    || parts[5].len() <= 5
    || arn.chars().any(char::is_whitespace)
  {
    return Err("Enter an IAM role ARN, for example arn:aws:iam::123456789012:role/platform-reader.".into());
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn update_uses_explicit_profile_and_separate_authentication_and_lookup_roles() {
    let context = Context {
      profile: "dev".into(),
      region: "eu-north-1".into(),
      executables: [("aws".into(), PathBuf::from("/test/aws"))].into(),
      original_environment: Default::default(),
      account: None,
    };
    let resource = Resource {
      id: "cluster-arn".into(),
      label: "dev-1".into(),
      columns: Vec::new(),
      current: false,
    };
    let settings = [
      ("alias".into(), "dev/eu-north-1/dev-1".into()),
      ("kubeconfig".into(), "/tmp/config with spaces".into()),
      ("role_arn".into(), "arn:aws:iam::123456789012:role/reader".into()),
      ("assume_role_arn".into(), "arn:aws:iam::123456789012:role/lookup".into()),
    ]
    .into();

    let command = update_command(&context, &resource, &settings).unwrap();
    let args: Vec<_> = command.get_args().map(|arg| arg.to_str().unwrap()).collect();

    for pair in [
      ["--profile", "dev"],
      ["--region", "eu-north-1"],
      ["--alias", "dev/eu-north-1/dev-1"],
      ["--user-alias", "dev/eu-north-1/dev-1"],
      ["--kubeconfig", "/tmp/config with spaces"],
      ["--role-arn", "arn:aws:iam::123456789012:role/reader"],
      ["--assume-role-arn", "arn:aws:iam::123456789012:role/lookup"],
    ] {
      assert!(args.windows(2).any(|window| window == pair));
    }
  }

  #[test]
  fn k9s_opens_the_written_context() {
    let context = Context {
      profile: "dev".into(),
      region: "eu-north-1".into(),
      executables: [("k9s".into(), PathBuf::from("/test/k9s"))].into(),
      original_environment: Default::default(),
      account: None,
    };
    let resource = Resource {
      id: "cluster-arn".into(),
      label: "dev-1".into(),
      columns: Vec::new(),
      current: false,
    };
    let settings = [
      ("alias".into(), "dev/eu-north-1/dev-1".into()),
      ("kubeconfig".into(), "/tmp/session/config".into()),
    ]
    .into();

    let command = Eks.launch(&context, &resource, &settings).unwrap();
    assert_eq!(command.get_program(), "/test/k9s");
    assert_eq!(
      command.get_args().collect::<Vec<_>>(),
      ["--context", "dev/eu-north-1/dev-1"]
    );
    assert!(command
      .get_envs()
      .any(|(name, value)| name == "KUBECONFIG" && value == Some("/tmp/session/config".as_ref())));
  }

  #[test]
  fn refuses_to_overwrite_an_alias_for_another_cluster_or_profile() {
    let view = value!({
      "contexts": [{"name": "dev", "context": {"cluster": "other", "user": "dev"}}],
      "users": [{"name": "dev", "user": {"exec": {"env": [{"name": "AWS_PROFILE", "value": "a"}]}}}],
    });

    assert!(validate_alias(&view, "dev", "selected", "a").is_err());
    assert!(validate_alias(&view, "new-name", "selected", "a").is_ok());
    assert!(validate_alias(&view, "dev", "other", "a").is_ok());
    assert!(validate_alias(&view, "dev", "other", "b").is_err());
  }

  #[test]
  fn validates_roles_including_other_aws_partitions() {
    for role in [
      "arn:aws:iam::123456789012:role/team/reader",
      "arn:aws-us-gov:iam::123456789012:role/reader",
    ] {
      assert!(validate_role(role).is_ok());
    }

    for role in [
      "reader",
      "arn:aws:sts::123456789012:assumed-role/reader/session",
      "arn:aws:iam::123:role/reader",
    ] {
      assert!(validate_role(role).is_err());
    }
  }
}

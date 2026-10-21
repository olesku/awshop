//! Shell sessions on EC2 instances through Systems Manager. No terminal code: the host
//! runs `aws ssm start-session` on the terminal.
use awshop_plugin::{account, json, Account, Context, Dependency, Outcome, Plugin, Resource, Result, Settings, Setup};
use serde_json::Value;
use std::collections::HashMap;

/// Lists the EC2 instances that Systems Manager manages and opens a shell on one.
pub struct Ssm;

impl Plugin for Ssm {
  fn name(&self) -> &'static str {
    "ssm"
  }

  fn title(&self) -> &'static str {
    "SSM"
  }

  fn description(&self) -> &'static str {
    "Open a shell on an EC2 instance through Systems Manager"
  }

  fn dependencies(&self) -> &'static [Dependency] {
    &[
      Dependency {
        name: "aws",
        installation: "Install AWS CLI v2 and add it to PATH.",
        optional: false,
      },
      Dependency {
        name: "session-manager-plugin",
        installation: "Install the Session Manager plugin for the AWS CLI: https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html",
        optional: false,
      },
    ]
  }

  fn discover(&self, context: &Context) -> Result<Vec<Resource>> {
    let Account { id: account, partition } = account(context)?;
    let managed = json(
      &mut context.aws("ssm", "describe-instance-information")?,
      "Systems Manager instance discovery",
    )?;
    let instances: Vec<&Value> = managed["InstanceInformationList"]
      .as_array()
      .ok_or("Systems Manager response did not contain an instance list.")?
      .iter()
      .filter(|instance| instance["ResourceType"] == "EC2Instance")
      .collect();
    let ids: Vec<&str> = instances
      .iter()
      .filter_map(|instance| instance["InstanceId"].as_str())
      .collect();

    let names = names(context, &ids);

    let mut resources: Vec<_> = instances
      .iter()
      .filter_map(|instance| {
        let id = instance["InstanceId"].as_str()?;
        let detail = [
          id,
          field(instance, "PlatformName"),
          field(instance, "IPAddress"),
          field(instance, "PingStatus"),
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        Some(Resource {
          id: format!("arn:{partition}:ec2:{}:{account}:instance/{id}", context.region),
          label: names.get(id).cloned().unwrap_or_else(|| id.to_owned()),
          detail,
          current: false,
        })
      })
      .collect();

    resources.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(resources)
  }

  fn prepare(&self, _context: &Context, _resource: &Resource, _saved: &Settings) -> Result<Setup> {
    Ok(Setup {
      action: "Start session".to_owned(),
      launch: None,
      fields: Vec::new(),
      notes: vec!["Opens a shell on the instance. Exit the shell to return.".to_owned()],
    })
  }

  fn validate_field(&self, field: &str, _value: &str) -> Result<()> {
    Err(format!("Unknown SSM setting '{field}'.").into())
  }

  fn execute(&self, context: &Context, resource: &Resource, _settings: &Settings) -> Result<Outcome> {
    let instance = resource
      .id
      .rsplit_once('/')
      .map(|(_, id)| id)
      .ok_or("Invalid instance ID.")?;

    let mut command = context.aws("ssm", "start-session")?;
    command.args(["--target", instance]);

    Ok(Outcome {
      message: format!("SSM session on {} ended.", resource.label),
      details: Vec::new(),
      open: Some(command),
    })
  }
}

/// Name tags by instance ID. Names only decorate the list, so instances are still
/// listed by ID when EC2 can't be read, for example without ec2:DescribeInstances.
fn names(context: &Context, ids: &[&str]) -> HashMap<String, String> {
  let mut names = HashMap::new();
  if ids.is_empty() {
    return names;
  }

  let Ok(mut command) = context.aws("ec2", "describe-instances") else {
    return names;
  };
  command.arg("--instance-ids").args(ids);
  let Ok(described) = json(&mut command, "EC2 instance lookup") else {
    return names;
  };

  let instances = described["Reservations"]
    .as_array()
    .into_iter()
    .flatten()
    .flat_map(|reservation| reservation["Instances"].as_array().into_iter().flatten());
  for instance in instances {
    let name = instance["Tags"]
      .as_array()
      .into_iter()
      .flatten()
      .find(|tag| tag["Key"] == "Name")
      .and_then(|tag| tag["Value"].as_str());
    if let (Some(id), Some(name)) = (instance["InstanceId"].as_str(), name) {
      names.insert(id.to_owned(), name.to_owned());
    }
  }

  names
}

/// A string field of a JSON object, or empty.
fn field<'a>(value: &'a Value, key: &str) -> &'a str {
  value[key].as_str().unwrap_or_default()
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  #[test]
  fn session_targets_the_instance_with_the_shown_profile_and_region() {
    let context = Context {
      profile: "dev".into(),
      region: "eu-north-1".into(),
      executables: [("aws".into(), PathBuf::from("/test/aws"))].into(),
      original_environment: Default::default(),
    };
    let resource = Resource {
      id: "arn:aws:ec2:eu-north-1:123456789012:instance/i-0abc".into(),
      label: "web-1".into(),
      detail: String::new(),
      current: false,
    };

    let outcome = Ssm.execute(&context, &resource, &Settings::new()).unwrap();
    let command = outcome.open.unwrap();
    let args: Vec<_> = command.get_args().map(|arg| arg.to_str().unwrap()).collect();

    for pair in [
      ["ssm", "start-session"],
      ["--target", "i-0abc"],
      ["--profile", "dev"],
      ["--region", "eu-north-1"],
    ] {
      assert!(args.windows(2).any(|window| window == pair), "{args:?}");
    }

    assert_eq!(outcome.message, "SSM session on web-1 ended.");
  }
}

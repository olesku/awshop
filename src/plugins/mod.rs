//! Built-in plugin registry. Adding a plugin does not change the wizard or shell functions.
use awshop_plugin::Plugin;

/// The plugin instances.
static EKS: awshop_eks::Eks = awshop_eks::Eks;
static SSM: awshop_ssm::Ssm = awshop_ssm::Ssm;

/// Every built-in plugin, in `awshop plugins` order.
pub fn all() -> [&'static dyn Plugin; 2] {
  [&EKS, &SSM]
}

/// The plugin whose command name is `name`.
pub fn find(name: &str) -> Option<&'static dyn Plugin> {
  all().iter().copied().find(|plugin| plugin.name() == name)
}

# SSM plugin

`awshop ssm` lists the EC2 instances that AWS Systems Manager manages in your account and region, and opens a shell on one with `aws ssm start-session`.

```sh
awshop ssm                  # choose an instance, open a shell on it
awshop ssm web              # start with the list filtered to "web"
awshop ssm --profile prod --region eu-west-1
awshop                      # or pick a profile and choose SSM in its menu
```

## Requirements

- **AWS CLI v2** and the [Session Manager plugin](https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html) in `PATH`. Without the plugin, SSM shows as unavailable in the profile's menu.
- **Instances:** the SSM Agent must run on the instance, and its instance profile must allow Systems Manager, for example with `AmazonSSMManagedInstanceCore`.
- **Permissions:** `sts:GetCallerIdentity` to find the account, `ssm:DescribeInstanceInformation` to list instances and `ssm:StartSession` to connect. `ec2:DescribeInstances` adds the `Name` tags; without it, instances are listed by ID.

## The list

| Column | Shows |
| --- | --- |
| Name | The instance's `Name` tag, or its ID |
| Detail | Instance ID, platform, private IP and the agent status, such as `Online` or `ConnectionLost` |

- **Only EC2 instances:** hybrid nodes that Systems Manager manages (`mi-…`) are left out.
- **Offline agents:** instances whose agent isn't `Online` are listed, but a session to them fails with the AWS CLI's error.

## The session

Press Enter on an instance to connect; there is nothing to configure first. awshop leaves its screen and gives the terminal to the session. Exit the shell, for example with `exit`, to return; awshop then confirms that the session ended. The session uses the profile and region shown in the menu. Run as `awshop ssm`, it doesn't change your shell's environment; chosen from a profile's menu, the profile is set in your shell when the session ends.

Ctrl-C inside the session goes to the remote shell, as in a plain `aws ssm start-session`; awshop keeps waiting until the session ends.

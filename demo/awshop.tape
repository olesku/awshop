# Record the demo: vhs demo/awshop.tape (needs `cargo build --release` first).
# Runs in demo/start.sh: made-up accounts and fake AWS tools, nothing real.
Output docs/demo.gif

Require zsh
Set Shell "bash"
Set FontFamily "Hack"
Set FontSize 17
Set Width 1180
Set Height 720
Set Padding 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 70ms
Set CursorBlink false

Hide
Type "exec ./demo/start.sh" Enter
Sleep 1.5s
Type "clear" Enter
Sleep 300ms
Show

# 1. Switch account: filter, Enter for the profile's menu, Enter to use it here.
#    awshop logs in with SSO when needed.
Sleep 800ms
Type "awshop" Sleep 400ms Enter
Sleep 1.8s
Type "platform-prod" Sleep 1s
Enter Sleep 1.4s
Enter
Sleep 3.5s

# 2. Straight into a cluster: the profile's menu, then EKS.
Type "awshop" Sleep 300ms Enter
Sleep 1.2s
Type "platform-dev" Sleep 900ms
Enter Sleep 1.2s
Down Sleep 500ms
Down Sleep 700ms
Enter Sleep 2s
Enter Sleep 1.8s
Enter Sleep 2.5s
Type "kubectl get nodes" Sleep 400ms Enter
Sleep 2.5s

# 3. A shell on an EC2 instance through SSM.
Type "awshop ssm" Sleep 400ms Enter
Sleep 2s
Type "web" Sleep 900ms
Enter Sleep 1.5s
Type "hostname" Sleep 300ms Enter
Sleep 1.2s
Type "exit" Sleep 300ms Enter
Sleep 2s

# 4. Leave the EKS session: KUBECONFIG goes back to what it was.
Type "awshop eks leave" Sleep 400ms Enter
Sleep 3s

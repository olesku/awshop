#!/usr/bin/env python3
"""Interactive regression checks using only Python's standard library (Unix).

Run `cargo build` followed by `python3 tests/eks_pty.py`.
AWS and kubectl are simulated; all config and preferences stay in a temp directory.
"""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / 'target/debug/awshop'
ANSI = re.compile(rb'\x1b\[[0-9;?]*[a-zA-Z]')

MOCK = r'''
import json, os, pathlib, sys, time
args = sys.argv[1:]
root = pathlib.Path(os.environ['AWSHOP_TEST_ROOT'])
name = pathlib.Path(sys.argv[0]).name
with (root/'calls.jsonl').open('a') as log:
    log.write(json.dumps({'tool':name, 'args':args})+'\n')
def value(flag):
    return args[args.index(flag)+1]
if name in ('xdg-open', 'browser'):
    page = pathlib.Path(args[-1])
    (root/'opened.json').write_text(json.dumps({'args':args, 'page':page.read_text(), 'mode':page.stat().st_mode & 0o777}))
elif name == 'k9s':
    (root/'k9s.json').write_text(json.dumps({'args':args, 'kubeconfig':os.environ.get('KUBECONFIG'), 'tty':os.isatty(0)}))
elif name == 'kubectl':
    # Without --kubeconfig, merge the KUBECONFIG files: first current-context wins.
    paths = [value('--kubeconfig')] if '--kubeconfig' in args else os.environ.get('KUBECONFIG', '').split(':')
    view = {'current-context':'', 'contexts':[], 'users':[]}
    for path in map(pathlib.Path, filter(None, paths)):
        if path.exists():
            config = json.loads(path.read_text())
            view['current-context'] = view['current-context'] or config.get('current-context', '')
            view['contexts'] += config.get('contexts', [])
            view['users'] += config.get('users', [])
    print(json.dumps(view))
elif args[:2] == ['sts', 'get-caller-identity']:
    if (root/'login-needed').exists():
        print('SSO token expired', file=sys.stderr)
        sys.exit(1)
    print(json.dumps({'Account':'123456789012', 'Arn':'arn:aws:sts::123456789012:assumed-role/test/session'}))
elif args[:2] == ['configure', 'export-credentials']:
    if (root/'fail-export').exists():
        print('Error when retrieving token from sso: Token has expired and refresh failed', file=sys.stderr)
        sys.exit(255)
    credentials = {'Version':1, 'AccessKeyId':'ASIAEXAMPLE', 'SecretAccessKey':'secret-key', 'SessionToken':'session-token'}
    if (root/'static-keys').exists():
        credentials = {'Version':1, 'AccessKeyId':'AKIAEXAMPLE', 'SecretAccessKey':'secret-key'}
    print(json.dumps(credentials))
elif args[:2] == ['sso', 'login']:
    (root/'login-needed').unlink()
    if (root/'slow-child').exists() and os.fork() == 0:
        time.sleep(10)
        os._exit(0)
    print('Successfully logged into SSO')
elif args[:2] == ['eks', 'list-clusters']:
    if (root/'slow-list').exists():
        time.sleep(float((root/'slow-list').read_text() or 5))
    if (root/'fail-list').exists():
        print('AccessDeniedException: eks:ListClusters is not allowed', file=sys.stderr)
        sys.exit(1)
    clusters = json.loads((root/'clusters').read_text()) if (root/'clusters').exists() else ['dev-1', 'tools-1']
    print(json.dumps({'clusters':[] if (root/'empty').exists() else clusters}))
elif args[:2] == ['eks', 'describe-cluster']:
    if (root/'fail-describe').exists():
        print('AccessDeniedException: eks:DescribeCluster is not allowed', file=sys.stderr)
        sys.exit(1)
    if (root/'slow-describe').exists():
        time.sleep(3)
    name = value('--name')
    print(json.dumps({'cluster':{'name':name, 'version':'1.31' if name == 'dev-1' else '1.30',
        'status':'ACTIVE' if name == 'dev-1' else 'UPDATING'}}))
elif args[:2] == ['ssm', 'describe-instance-information']:
    print(json.dumps({'InstanceInformationList':[
        {'InstanceId':'i-0abc', 'ResourceType':'EC2Instance', 'PingStatus':'Online', 'PlatformName':'Amazon Linux', 'IPAddress':'10.0.0.1'},
        {'InstanceId':'i-0def', 'ResourceType':'EC2Instance', 'PingStatus':'ConnectionLost'},
        {'InstanceId':'mi-0123', 'ResourceType':'ManagedInstance', 'PingStatus':'Online'}]}))
elif args[:2] == ['ec2', 'describe-instances']:
    print(json.dumps({'Reservations':[{'Instances':[{'InstanceId':'i-0abc', 'Tags':[{'Key':'Name', 'Value':'web-1'}]}]}]}))
elif args[:2] == ['ssm', 'start-session']:
    (root/'ssm.json').write_text(json.dumps({'args':args, 'tty':os.isatty(0)}))
    if (root/'ssm-shell').exists():
        # Like the AWS CLI, which passes Ctrl-C on to the remote shell.
        import signal
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        for line in sys.stdin:
            if line.strip() == 'exit':
                break
elif args[:2] == ['eks', 'update-kubeconfig']:
    if (root/'fail-update').exists():
        print('AccessDeniedException: eks:DescribeCluster is not allowed', file=sys.stderr)
        sys.exit(1)
    path = pathlib.Path(value('--kubeconfig'))
    path.parent.mkdir(parents=True, exist_ok=True)
    alias = value('--alias')
    arn = 'arn:aws:eks:'+value('--region')+':123456789012:cluster/'+value('--name')
    role_args = ['--role-arn', value('--role-arn')] if '--role-arn' in args else []
    path.write_text(json.dumps({'current-context':alias,
        'contexts':[{'name':alias, 'context':{'cluster':arn, 'user':alias}}],
        'users':[{'name':alias, 'user':{'exec':{'args':role_args, 'env':[{'name':'AWS_PROFILE', 'value':value('--profile')}]}}}]}, indent=2))
    print('Updated context '+alias)
else:
    print('Unexpected command: '+repr(args), file=sys.stderr)
    sys.exit(2)
'''

class Fixture:
    def __init__(self, directory):
        self.root = Path(directory)
        self.bin = self.root/'bin'
        self.bin.mkdir()
        (self.root/'tmp').mkdir()
        (self.root/'run').mkdir(mode=0o700)
        self.config = self.root/'aws-config'
        self.config.write_text('[profile dev]\nregion=eu-north-1\n[profile prod]\nregion=us-east-1\n')
        for name in ('aws', 'kubectl', 'session-manager-plugin', 'xdg-open'):
            path = self.bin/name
            path.write_text('#!'+sys.executable+' -I\n'+MOCK)
            path.chmod(0o755)
        (self.bin/'awshop').symlink_to(BINARY)
        # The fish integration reads fd 3 through a temporary file.
        for name in ('mktemp', 'rm'):
            (self.bin/name).symlink_to(shutil.which(name))
        # Nothing from the developer's environment: only what the tests set.
        self.env = dict(HOME=str(self.root), XDG_CONFIG_HOME=str(self.root/'config'), XDG_STATE_HOME=str(self.root/'state'), XDG_CACHE_HOME=str(self.root/'cache'),
            TMPDIR=str(self.root/'tmp'), XDG_RUNTIME_DIR=str(self.root/'run'), AWSHOP_TEST_ROOT=str(self.root), AWS_CONFIG_FILE=str(self.config),
            AWS_SHARED_CREDENTIALS_FILE=str(self.root/'credentials'), AWS_PROFILE='dev',
            KUBECONFIG=str(self.root/'kubeconfig'), PATH=str(self.bin), TERM='xterm-256color', LANG='C.UTF-8')
    def sessions(self):
        root = self.root/'run/awshop/sessions'
        return sorted(root.iterdir()) if root.exists() else []
    def k9s(self):
        (self.bin/'k9s').write_text((self.bin/'aws').read_text())
        (self.bin/'k9s').chmod(0o755)
        return self.root/'k9s.json'
    def calls(self):
        path = self.root/'calls.jsonl'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []
    def updates(self):
        return [call['args'] for call in self.calls() if call['args'][:2] == ['eks', 'update-kubeconfig']]
    def preferences(self):
        return self.root/'state/awshop/preferences.json'
    def awshop_config(self, text):
        path = self.root/'config/awshop/config.toml'
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

class Session:
    def __init__(self, fixture, args=None, command=None):
        self.fixture = fixture
        self.output = b''
        self.latest = b''
        self.stdout, writer = os.pipe()
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.close(self.stdout)
            os.dup2(writer, 1)
            os.close(writer)
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', 26, 110, 0, 0))
            argv = command or [str(BINARY), 'eks', '--no-login', '--saved', *(args or [])]
            os.execvpe(argv[0], argv, fixture.env)
        os.close(writer)
    def read(self, timeout=.05):
        if select.select([self.fd], [], [], timeout)[0]:
            try:
                chunk = os.read(self.fd, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    return False
                raise
            self.output += chunk
            self.latest += chunk
            return bool(chunk)
        return False
    def wait(self, *texts):
        deadline = time.monotonic()+5
        while time.monotonic() < deadline:
            self.read()
            frame = ANSI.sub(b'', self.latest.rsplit(b'\x1b[2J', 1)[-1]).decode(errors='replace')
            if all(text in frame for text in texts):
                while self.read():
                    pass
                return
        self.abort()
        raise AssertionError('Missing '+repr(texts)+'\n'+ANSI.sub(b'', self.latest).decode(errors='replace'))
    def frame(self):
        return ANSI.sub(b'', self.output.rsplit(b'\x1b[2J', 1)[-1]).decode(errors='replace')
    def key(self, key, *texts):
        self.latest = b''
        os.write(self.fd, key)
        if texts:
            self.wait(*texts)
    def finish(self, key=None):
        if key:
            self.key(key)
        deadline = time.monotonic()+5
        while time.monotonic() < deadline:
            self.read()
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                while self.read():
                    pass
                stdout = os.read(self.stdout, 65536).decode()
                flags = termios.tcgetattr(self.fd)[3]
                assert flags & termios.ICANON and flags & termios.ECHO, 'terminal was left in raw mode'
                assert os.waitstatus_to_exitcode(status) == 0, self.output.decode(errors='replace')
                os.close(self.stdout)
                os.close(self.fd)
                return stdout
        self.abort()
        raise AssertionError('process did not exit')
    def abort(self):
        try:
            os.killpg(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        except ProcessLookupError:
            pass


def test_missing_kubectl(f):
    (f.bin/'kubectl').unlink()
    result = subprocess.run([str(BINARY), 'eks'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 1 and "requires 'kubectl' in PATH" in result.stderr
    assert not f.calls(), 'dependencies must be checked before calling AWS'


def test_missing_aws(f):
    (f.bin/'aws').unlink()
    result = subprocess.run([str(BINARY), 'eks'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 1 and "requires 'aws' in PATH" in result.stderr


def test_sso_before_discovery(f):
    f.config.write_text('[profile dev]\nregion=eu-north-1\nsso_session=my-sso\n[sso-session my-sso]\nsso_region=eu-north-1\n')
    (f.root/'login-needed').touch()
    s = Session(f, command=[str(BINARY), 'eks'])
    s.wait('dev-1')
    assert s.finish(b'\x1b') == ''
    operations = [call['args'][:2] for call in f.calls()]
    assert operations[:4] == [['sts', 'get-caller-identity'], ['sso', 'login'], ['sts', 'get-caller-identity'], ['eks', 'list-clusters']]
    assert 'Successfully logged into SSO' in s.output.decode()


def test_login_check_gives_the_account(f):
    f.config.write_text('[profile dev]\nregion=eu-north-1\nsso_session=my-sso\n[sso-session my-sso]\nsso_region=eu-north-1\n')
    for plugin in ['eks', 'ssm']:
        s = Session(f, command=[str(BINARY), plugin])
        s.wait('awshop › '+plugin.upper(), '› ')
        s.finish(b'\x1b')
    # One identity lookup each: the login check's answer also gives the plugin the account.
    operations = [call['args'][:2] for call in f.calls() if call['tool'] == 'aws']
    assert operations.count(['sts', 'get-caller-identity']) == 2, operations


def test_cancel(f):
    s = Session(f)
    s.wait('dev-1', 'tools-1')
    s.key(b'\r', 'Configure and activate', 'Authentication')
    assert s.finish(b'\x03') == ''
    assert not f.updates() and not f.preferences().exists()
    assert not (f.root/'kubeconfig').exists()


def test_default_and_remembered_setup(f):
    s = Session(f)
    s.wait('dev-1', 'tools-1')
    s.key(b'\r', 'Configure and activate', 'dev/eu-north-1/dev-1')
    assert s.finish(b'\r') == ''
    assert 'Activated Kubernetes context dev/eu-north-1/dev-1' in s.output.decode()
    args = f.updates()[-1]
    for flag, value in [('--profile', 'dev'), ('--region', 'eu-north-1'), ('--name', 'dev-1'),
            ('--alias', 'dev/eu-north-1/dev-1'), ('--kubeconfig', str(f.root/'kubeconfig'))]:
        assert args[args.index(flag)+1] == value
    assert '--role-arn' not in args and '--assume-role-arn' not in args
    assert f.preferences().exists()
    s = Session(f)
    s.wait('dev-1 *')
    s.key(b'\r', 'Update and activate')
    assert s.finish(b'\x03') == ''
    assert len(f.updates()) == 1


def test_role_and_custom_path(f):
    role = 'arn:aws:iam::123456789012:role/platform-reader'
    s = Session(f)
    s.wait('dev-1')
    s.key(b'\r', 'Configure and activate')
    s.key(b'\x1b[B'*2+b'\r', 'Enter a new value')
    s.key(b'\x1b[B\r', 'Role ARN for kubectl authentication')
    s.key(role.encode()+b'\r', 'Save this value as')
    s.key(b'\r', 'Configure and activate', 'platform-reader')
    # Change context name, then the destination file.
    s.key(b'\x1b[B\r', 'Context name', 'ctrl-u clear')
    s.key(b'\x15friendly-dev\r', 'Configure and activate', 'friendly-dev')
    s.key(b'\x1b[B\r', 'Kubeconfig', 'ctrl-u clear')
    custom = f.root/'custom config'
    s.key(b'\x15'+str(custom).encode()+b'\r', 'Configure and activate', str(custom))
    s.key(b'\x1b[A'*4, '› Configure and activate')
    s.finish(b'\r')
    args = f.updates()[-1]
    assert args[args.index('--role-arn')+1] == role
    assert args[args.index('--kubeconfig')+1] == str(custom)
    assert args[args.index('--alias')+1] == 'friendly-dev'
    saved = json.loads(f.preferences().read_text())
    assert saved['choices']['eks']['iam-roles']['platform-reader'] == role
    s = Session(f)
    s.wait('dev-1')
    s.key(b'\r', 'Update and activate', 'friendly-dev', 'platform-reader', str(custom))
    s.finish(b'\x03')


def test_config_scope_and_roles(f):
    reader = 'arn:aws:iam::123456789012:role/reader'
    f.awshop_config('[eks]\nscope = "saved"\n\n[eks.choices.iam-roles]\nteam-reader = "'+reader+'"\n')
    s = Session(f, command=[str(BINARY), 'eks', '--no-login'])
    s.wait('dev-1')
    s.key(b'\r', 'Configure and activate', 'Saved config')
    s.key(b'\x1b[B'*2+b'\r', 'team-reader')
    s.key(b'\x1b[B\r', 'Configure and activate', 'team-reader ('+reader+')')
    s.key(b'\x1b[A'*2)
    s.finish(b'\r')
    args = f.updates()[-1]
    assert args[args.index('--role-arn')+1] == reader
    # Configured values are offered, not copied into state.
    assert 'team-reader' not in f.preferences().read_text()


def test_invalid_config_is_reported(f):
    f.awshop_config('[eks]\nscop = "saved"\n')
    result = subprocess.run([str(BINARY), 'eks', '--no-login'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 1 and 'config.toml' in result.stderr and 'scop' in result.stderr


def test_legacy_preferences_move_to_state(f):
    legacy = f.root/'config/awshop/preferences.json'
    legacy.parent.mkdir(parents=True)
    arn = 'arn:aws:eks:eu-north-1:123456789012:cluster/dev-1'
    legacy.write_text(json.dumps({'plugins': {'eks': {'dev': {arn: {'alias': 'kept-alias'}}}}}))
    s = Session(f)
    s.wait('dev-1')
    s.key(b'\r', 'Configure and activate', 'kept-alias')
    s.finish(b'\r')
    assert not legacy.exists()
    assert json.loads(f.preferences().read_text())['plugins']['eks']['dev'][arn]['alias'] == 'kept-alias'


def test_profile_and_region(f):
    s = Session(f)
    s.wait('dev-1')
    s.key(b'\x10', 'awshop › AWS profile')
    s.key(b'/prod\r', 'dev-1', 'prod', 'us-east-1')
    s.key(b'\x12', 'awshop › AWS region')
    s.key(b'\x1b[B\r', 'Region (for example')
    s.key(b'\x15eu-west-1\r', 'dev-1', 'prod', 'eu-west-1')
    s.key(b'\r', 'Configure and activate')
    s.finish(b'\r')
    args = f.updates()[-1]
    assert args[args.index('--profile')+1] == 'prod'
    assert args[args.index('--region')+1] == 'eu-west-1'


def test_errors_are_not_empty_lists(f):
    (f.root/'fail-list').touch()
    s = Session(f)
    s.wait('Could not load resources', 'AccessDeniedException')
    s.finish(b'\x03')
    assert not f.updates() and not f.preferences().exists()


def test_failed_update_does_not_save_preferences(f):
    (f.root/'fail-update').touch()
    s = Session(f)
    s.wait('dev-1')
    s.key(b'\r', 'Configure and activate')
    s.key(b'\r', 'Setup failed', 'AccessDeniedException')
    s.finish(b'\x03')
    assert not f.preferences().exists()


def test_empty_and_resize(f):
    (f.root/'empty').touch()
    s = Session(f)
    s.wait('No matching resources')
    # Only a frame drawn after the resize counts.
    s.latest = b''
    fcntl.ioctl(s.fd, termios.TIOCSWINSZ, struct.pack('HHHH', 8, 30, 0, 0))
    s.wait('awshop › EKS')
    s.finish(b'\x1b')
    assert not f.updates()


def test_zsh_plugin_dispatch(f):
    zsh = '/usr/bin/zsh'
    if not Path(zsh).exists():
        print('SKIP: zsh is not installed')
        return
    command = [zsh, '-f', '-c', f'source {ROOT}/completions/awshop.zsh; awshop eks --no-login; print -r -- "PROFILE=$AWS_PROFILE"']
    s = Session(f, command=command)
    s.wait('dev-1')
    s.key(b'\r', 'Start EKS session', 'This shell (temporary)')
    assert s.finish(b'\r') == 'PROFILE=dev\n'
    assert 'Set environment to' not in s.output.decode()


def test_shared_profile_selector(f):
    s = Session(f, command=[str(BINARY), '--no-login'])
    s.wait('AWS profile selector', 'dev *')
    s.key(b'/prod', '› prod')
    assert s.finish(b'\r') == 'prod\n'
    s = Session(f, command=[str(BINARY), '--no-login'])
    s.wait('AWS profile selector')
    assert s.finish(b'\x03') == ''


def test_cluster_versions_and_status(f):
    s = Session(f)
    s.wait('Cluster  Version  Status', 'dev-1    v1.31    ACTIVE', 'tools-1  v1.30    UPDATING')
    s.finish(b'\x1b')
    # Without eks:DescribeCluster the list still works, and versions from the last
    # run don't stay once the fresh list is shown.
    (f.root/'fail-describe').touch()
    s = Session(f)
    s.wait('dev-1', 'tools-1', '^L reload')
    time.sleep(2)
    s.read()
    assert 'v1.3' not in s.frame() and 'Version' not in s.frame() and 'Waiting for AWS' not in s.frame()
    s.finish(b'\x1b')


def test_list_shows_before_versions_arrive(f):
    (f.root/'slow-describe').touch()
    started = time.monotonic()
    s = Session(f)
    s.wait('dev-1', 'tools-1', '^L reload')
    assert time.monotonic()-started < 2.5 and 'v1.3' not in ANSI.sub(b'', s.output).decode()
    s.wait('dev-1    v1.31    ACTIVE', 'tools-1  v1.30    UPDATING')
    s.finish(b'\x1b')


def test_current_cluster_is_marked_across_kubeconfig_files(f):
    # The current context is in the second file, for the second cluster in the list.
    arn = 'arn:aws:eks:eu-north-1:123456789012:cluster/tools-1'
    (f.root/'first').write_text('{}')
    (f.root/'second').write_text(json.dumps({'current-context':'tools',
        'contexts':[{'name':'tools', 'context':{'cluster':arn, 'user':'tools'}}]}))
    f.env['KUBECONFIG'] = f'{f.root}/first:{f.root}/second'
    s = Session(f)
    s.wait('* marks the current one', '  dev-1', '› tools-1 *')
    assert 'dev-1 *' not in ANSI.sub(b'', s.output).decode()
    s.finish(b'\x1b')


def remember_list(f, clusters):
    """Open the list once so it is cached, then make AWS answer slowly with `clusters`."""
    s = Session(f)
    s.wait('dev-1', 'tools-1', '^L reload')
    s.finish(b'\x1b')
    (f.root/'clusters').write_text(json.dumps(clusters))
    (f.root/'slow-list').write_text('3')


def test_last_list_shows_while_aws_answers(f):
    remember_list(f, ['dev-1', 'new-1'])
    cache = f.root/'cache/awshop/eks.json'
    assert cache.stat().st_mode & 0o777 == 0o600
    started = time.monotonic()
    s = Session(f)
    s.wait('tools-1', 'Last loaded just now · Waiting for AWS…')
    assert time.monotonic()-started < 2
    # It is said below the list, so the rows don't move when the fresh list comes.
    assert s.frame().index('tools-1') < s.frame().index('Last loaded') < s.frame().index('↑/↓')
    # The fresh list replaces it, with the search typed meanwhile.
    s.key(b'/1', 'Search: 1')
    s.wait('Search: 1', 'new-1')
    assert 'tools-1' not in s.frame() and 'Waiting for AWS' not in s.frame()
    s.key(b'\x1b', 'Search: 1')
    s.finish(b'\x1b')
    assert 'new-1' in cache.read_text() and 'tools-1' not in cache.read_text()

    # Lists are per profile and region.
    s = Session(f, ['--region', 'us-east-1'])
    s.wait('Waiting for AWS…')
    assert 'new-1' not in s.frame()
    s.wait('new-1')
    s.finish(b'\x1b')


def test_choice_from_the_last_list_waits_for_aws(f):
    remember_list(f, ['dev-1'])
    s = Session(f)
    s.wait('tools-1', 'Waiting for AWS…')
    # A cluster that is gone isn't set up; the fresh list shows instead.
    s.key(b'\x1b[B\r', 'tools-1 is no longer in the list from AWS.')
    s.key(b'\x1b', '› dev-1')
    assert 'tools-1' not in s.frame()
    s.finish(b'\x1b')

    s = Session(f)
    s.wait('dev-1', 'Waiting for AWS…')
    s.key(b'\r', 'Configure and activate', 'dev/eu-north-1/dev-1')
    assert s.finish(b'\r') == ''
    assert f.updates()[-1][f.updates()[-1].index('--name')+1] == 'dev-1'


def test_going_back_after_a_choice_shows_the_fresh_list(f):
    remember_list(f, ['dev-1'])
    cache = f.root/'cache/awshop/eks.json'
    s = Session(f)
    s.wait('tools-1', 'Waiting for AWS…')
    s.key(b'\r', 'Configure and activate')
    # The fresh list is saved at once, since a setup that succeeds doesn't come back.
    assert 'tools-1' not in cache.read_text()
    s.key(b'\x1b', '› dev-1', '^L reload')
    assert 'tools-1' not in s.frame() and 'Waiting for AWS' not in s.frame()
    s.finish(b'\x1b')
    lists = [call for call in f.calls() if call['args'][:2] == ['eks', 'list-clusters']]
    assert len(lists) == 2, lists


def test_highlight_is_kept_when_the_fresh_list_comes(f):
    remember_list(f, ['a-1', 'dev-1', 'tools-1'])
    s = Session(f)
    s.wait('Waiting for AWS…')
    s.key(b'\x1b[B', '› tools-1')
    # The new cluster comes first, and the highlight stays on the same cluster.
    s.wait('a-1', '› tools-1')
    s.finish(b'\x1b')


def test_cached_columns_do_not_outlive_a_failing_lookup(f):
    s = Session(f)
    s.wait('dev-1    v1.31')
    s.finish(b'\x1b')
    (f.root/'fail-describe').touch()
    s = Session(f)
    s.wait('Waiting for AWS…', 'v1.31')
    time.sleep(2)
    s.read()
    assert 'Version' not in s.frame()
    # Even when left with Ctrl-C, the cache keeps what the lookup found.
    s.finish(b'\x03')
    assert 'v1.31' not in (f.root/'cache/awshop/eks.json').read_text()


def test_empty_last_list_is_not_shown(f):
    (f.root/'empty').touch()
    s = Session(f)
    s.wait('No matching resources')
    s.finish(b'\x1b')
    (f.root/'empty').unlink()
    (f.root/'slow-list').write_text('2')
    s = Session(f)
    s.wait('Waiting for AWS…', '^C cancel')
    assert 'No matching' not in s.frame()
    s.wait('dev-1', 'tools-1')
    s.finish(b'\x1b')


def test_current_cluster_is_not_taken_from_the_last_list(f):
    remember_list(f, ['dev-1', 'tools-1'])
    arn = 'arn:aws:eks:eu-north-1:123456789012:cluster/tools-1'
    (f.root/'kubeconfig').write_text(json.dumps({'current-context':'tools',
        'contexts':[{'name':'tools', 'context':{'cluster':arn, 'user':'tools'}}]}))
    s = Session(f)
    s.wait('› tools-1 *', 'Waiting for AWS…')
    s.finish(b'\x1b')


def test_failed_refresh_shows_the_error(f):
    remember_list(f, ['dev-1', 'tools-1'])
    (f.root/'fail-list').touch()
    s = Session(f)
    s.wait('tools-1', 'Waiting for AWS…')
    s.wait('Could not load resources', 'eks:ListClusters is not allowed')
    s.finish(b'\x03')


def test_expired_login_with_a_last_list(f):
    f.config.write_text('[profile dev]\nregion=eu-north-1\nsso_session=my-sso\n[sso-session my-sso]\nsso_region=eu-north-1\n')
    s = Session(f, command=[str(BINARY), 'eks'])
    s.wait('dev-1', 'tools-1', '^L reload')
    s.finish(b'\x1b')
    (f.root/'login-needed').touch()
    calls = len(f.calls())
    s = Session(f, command=[str(BINARY), 'eks'])
    s.wait('tools-1', 'Waiting for AWS…')
    # The background check never logs in; the login runs as without a cache.
    s.wait('dev-1', '^L reload')
    s.finish(b'\x1b')
    operations = [call['args'][:2] for call in f.calls()[calls:] if call['tool'] == 'aws']
    assert operations[:5] == [['sts', 'get-caller-identity'], ['sts', 'get-caller-identity'], ['sso', 'login'],
        ['sts', 'get-caller-identity'], ['eks', 'list-clusters']], operations
    assert 'Successfully logged into SSO' in s.output.decode()


def test_cache_can_be_turned_off(f):
    f.awshop_config('[eks]\ncache = false\n')
    remember_list(f, ['dev-1', 'tools-1'])
    s = Session(f)
    s.wait('Waiting for AWS…')
    assert 'dev-1' not in s.frame()
    s.wait('dev-1', 'tools-1')
    s.finish(b'\x1b')
    assert not (f.root/'cache/awshop/eks.json').exists()


def test_alias_of_another_profile_is_refused(f):
    s = Session(f)
    s.wait('dev-1')
    s.key(b'\r', 'Configure and activate')
    s.finish(b'\r')
    s = Session(f, args=['--profile', 'prod', '--region', 'eu-north-1'])
    s.wait('dev-1 *')
    s.key(b'\r', 'Update and activate', 'prod/eu-north-1/dev-1')
    s.key(b'\x1b[B'*3+b'\r', 'Context name', 'ctrl-u clear')
    s.key(b'\x15dev/eu-north-1/dev-1\r', 'Update and activate', 'dev/eu-north-1/dev-1')
    s.key(b'\x1b[A'*3+b'\r', 'Setup failed', 'already used by another cluster or AWS profile')
    s.finish(b'\x03')
    assert len(f.updates()) == 1


def test_version(f):
    result = subprocess.run([str(BINARY), '--version'], env=f.env, text=True, capture_output=True)
    version = re.search(r'^version = "(.*)"$', (ROOT/'Cargo.toml').read_text(), re.M).group(1)
    assert result.returncode == 0 and result.stdout == f'awshop {version}\n'


def test_unknown_selector_option_is_refused(f):
    result = subprocess.run([str(BINARY), '--no-logn'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 1 and "Unknown option '--no-logn'" in result.stderr
    assert not f.calls()


def test_ctrl_c_in_an_ssm_session_reaches_only_the_session(f):
    (f.root/'ssm-shell').touch()
    s = Session(f, command=[str(BINARY), 'ssm', '--no-login', 'web'])
    s.wait('› web-1')
    s.key(b'\r')
    time.sleep(.5)
    # Ctrl-C goes to the remote shell; awshop keeps waiting for the session.
    os.write(s.fd, b'\x03')
    time.sleep(.5)
    assert os.waitpid(s.pid, os.WNOHANG) == (0, 0)
    os.write(s.fd, b'exit\n')
    assert s.finish() == ''
    assert 'SSM session on web-1 ended.' in s.output.decode()
    assert 'web-1' in (f.root/'cache/awshop/ssm.json').read_text()


def test_narrow_terminal_drops_the_logo(f):
    s = Session(f, command=[str(BINARY), '--no-login'])
    s.wait('AWS profile selector', 'dev *')
    s.latest = b''
    fcntl.ioctl(s.fd, termios.TIOCSWINSZ, struct.pack('HHHH', 26, 30, 0, 0))
    s.wait('dev *')
    assert '████' not in ANSI.sub(b'', s.latest).decode()
    s.finish(b'\x1b')


def test_add_refuses_an_existing_name(f):
    result = subprocess.run([str(BINARY), 'add', 'dev'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 1 and "AWS profile 'dev' already exists" in result.stderr
    result = subprocess.run([str(BINARY), 'add', 'a', 'b'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 1 and 'Usage: awshop add [profile-name]' in result.stderr
    assert not f.calls()


def test_doctor_reports_session_problems(f):
    f.config.write_text('[profile a]\nsso_session = missing\n[profile b]\nsource_profile = a\n'
        '[sso-session corp]\nsso_start_url = https://example.awsapps.com/start\nsso_region = eu-north-1\n')
    result = subprocess.run([str(BINARY), '--doctor'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 0, result.stderr
    for line in ['Profiles: 2', 'Unused SSO sessions: corp', "Profile 'a' references an undefined SSO session.",
            'Role chain: b uses source profile a']:
        assert line in result.stdout, result.stdout
    assert 'example.awsapps.com' not in result.stdout and not f.calls()


def test_leave_and_cleanup_without_the_shell_function(f):
    result = subprocess.run([str(BINARY), 'eks', 'leave'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 1 and 'Use the awshop shell function' in result.stderr
    result = subprocess.run([str(BINARY), '__session-cleanup', 'abc'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 1 and 'Usage: awshop __session-cleanup PID' in result.stderr


def test_help_after_a_command_only_prints_help(f):
    for command in ['sync', 'plugins', 'init', 'add', 'console']:
        result = subprocess.run([str(BINARY), command, '--help'], env=f.env, text=True, capture_output=True)
        assert result.returncode == 0 and result.stdout.startswith('awshop [--no-login]'), (command, result)
    assert not f.calls()


def test_ctrl_c_while_waiting_restores_the_terminal(f):
    (f.root/'slow-list').touch()
    s = Session(f)
    s.wait('Waiting for AWS')
    started = time.monotonic()
    os.write(s.fd, b'\x03')
    _, status = os.waitpid(s.pid, 0)
    while s.read():
        pass
    assert os.waitstatus_to_exitcode(status) == 130 and time.monotonic()-started < 2
    assert s.output.rstrip(b'\r\n').endswith(b'\x1b[?1049l\x1b[?25h\x1b[0m'), s.output[-60:]


def test_left_arrow_does_not_leave_the_first_menu(f):
    for command, first, second in [([str(BINARY), '--no-login'], '› dev', '› prod'),
            ([str(BINARY), 'eks', '--no-login'], '› dev-1', '› tools-1')]:
        s = Session(f, command=command)
        s.wait(first)
        # Still running after ←: the next key moves the selection.
        s.key(b'\x1b[D')
        s.key(b'\x1b[B', second)
        assert '←' not in ANSI.sub(b'', s.output).decode()
        assert s.finish(b'\x03') == ''


def test_saved_setup_opens_k9s(f):
    k9s = f.k9s()
    s = Session(f)
    s.wait('dev-1')
    s.key(b'\r', 'Configure and activate', 'Configure and open k9s')
    s.key(b'\x1b[B', '› Configure and open k9s', 'Quitting k9s returns you')
    s.finish(b'\r')
    assert json.loads(k9s.read_text()) == {'args':['--context', 'dev/eu-north-1/dev-1'],
        'kubeconfig':str(f.root/'kubeconfig'), 'tty':True}
    assert 'Activated Kubernetes context' in s.output.decode()


def test_recent_profiles_come_first(f):
    s = Session(f, command=[str(BINARY), '--no-login'])
    s.wait('AWS profile selector', '› dev *')
    s.key(b'/prod', '› prod')
    assert s.finish(b'\r') == 'prod\n'
    assert json.loads(f.preferences().read_text())['recent_profiles'] == ['prod']
    s = Session(f, command=[str(BINARY), '--no-login'])
    s.wait('AWS profile selector', '› dev *')
    frame = ANSI.sub(b'', s.latest.rsplit(b'\x1b[2J', 1)[-1]).decode(errors='replace')
    assert frame.index('prod') < frame.index('dev *'), frame
    s.finish(b'\x03')


def test_ssm_lists_managed_instances_and_opens_a_session(f):
    s = Session(f, command=[str(BINARY), 'ssm', '--no-login'])
    s.wait('awshop › SSM', 'Name    Instance  Platform      IP address  Agent',
        'web-1   i-0abc    Amazon Linux  10.0.0.1    Online', 'i-0def  i-0def                              ConnectionLost')
    assert 'mi-0123' not in ANSI.sub(b'', s.output).decode()
    s.key(b'/web', '› web-1')
    # Nothing to configure, so Enter on the instance starts the session.
    assert s.finish(b'\r') == ''
    assert 'Start session' not in ANSI.sub(b'', s.output).decode()
    session = json.loads((f.root/'ssm.json').read_text())
    assert session['tty'] and session['args'][:4] == ['ssm', 'start-session', '--profile', 'dev']
    assert session['args'][session['args'].index('--target')+1] == 'i-0abc'
    assert 'SSM session on web-1 ended.' in s.output.decode()


def test_without_the_shell_function_enter_uses_the_profile(f):
    # No menu: scripts get the profile name at once, and → does nothing.
    s = Session(f, command=[str(BINARY), '--no-login'])
    s.wait('AWS profile selector', 'dev *')
    s.key(b'\x1b[C')
    assert s.finish(b'\r') == 'dev\n'


def test_missing_session_manager_plugin(f):
    (f.bin/'session-manager-plugin').unlink()
    result = subprocess.run([str(BINARY), 'ssm'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 1 and "requires 'session-manager-plugin' in PATH" in result.stderr


def test_page_and_end_keys(f):
    f.config.write_text(''.join(f'[profile p-{index:02}]\nregion=eu-north-1\n' for index in range(40)))
    s = Session(f, command=[str(BINARY), '--no-login'])
    # Columns that no profile has a value for are left out.
    s.wait('AWS profile selector', 'Profile  Region', '› p-00')
    # A page is the 13 rows that fit below the logo and headings in this 26-row terminal.
    s.key(b'\x1b[6~', '› p-13')
    s.key(b'\x1b[6~', '› p-26')
    s.key(b'\x1b[F', '› p-39')
    s.key(b'\x1b[5~', '› p-26')
    s.key(b'\x1b[H', '› p-00')
    s.key(b'\x1b[5~', '› p-00')
    s.finish(b'\x1b')


def test_zsh_profile_dispatch(f):
    if not Path('/usr/bin/zsh').exists():
        print('SKIP: zsh is not installed')
        return
    command = ['/usr/bin/zsh', '-f', '-c', f'source {ROOT}/completions/awshop.zsh; awshop --no-login; print -r -- "PROFILE=$AWS_PROFILE"']
    s = Session(f, command=command)
    s.wait('AWS profile selector')
    s.key(b'/prod', '› prod')
    s.key(b'\r', '› Use in this shell')
    assert s.finish(b'\r') == 'PROFILE=prod\n'
    assert 'Set environment to prod' in s.output.decode()


if __name__ == '__main__':
    tests = [value for name, value in list(globals().items()) if name.startswith('test_')]
    for test in tests:
        with tempfile.TemporaryDirectory(prefix='awshop-eks-test-') as directory:
            test(Fixture(directory))
        print('PASS:', test.__name__)

#!/usr/bin/env python3
"""SSO account sync against a fake SSO portal. Run `cargo build` first."""
import hashlib
import shutil
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import subprocess
import tempfile
import threading
import time
from urllib.parse import parse_qs, urlparse
from eks_pty import ANSI, BINARY, ROOT, Fixture, Session

OWN = '''# Hand-written, kept as is.
[sso-session corp]
sso_start_url = https://corp.awsapps.com/start/
sso_region = eu-north-1

[profile dev]
sso_session = corp
sso_account_id = 111111111111
sso_role_name = Admin
region = eu-north-1

[profile prod]
region = us-east-1
'''
ACCOUNTS = [
    {'accountId': '111111111111', 'accountName': 'Dev', 'emailAddress': 'dev@example.com'},
    {'accountId': '222222222222', 'accountName': 'Team Data (production)', 'emailAddress': 'data@example.com'},
    {'accountId': '333333333333', 'accountName': 'Prod', 'emailAddress': 'prod@example.com'},
]
ROLES = {'111111111111': ['Admin'], '222222222222': ['Admin', 'ReadOnly'], '333333333333': ['Admin']}


class Portal:
    """Serves the two SSO portal calls awshop makes, in two pages each where it can.
    `failures` answers that many requests with 503 first; `status` answers all with it."""
    def __init__(self, delay=0, failures=0, status=None):
        self.requests = []
        self.failures = failures
        self.status = status
        self.token = 'test-token'
        self.accounts = list(ACCOUNTS)
        portal = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_GET(self):
                time.sleep(delay)
                url = urlparse(self.path)
                query = {key: values[0] for key, values in parse_qs(url.query).items()}
                portal.requests.append((url.path, query))
                if portal.failures:
                    portal.failures -= 1
                    return self.reply(503, {'message': 'Service unavailable'})
                if portal.status:
                    return self.reply(portal.status, {'message': 'Failed'})
                if url.path == '/federation':
                    return self.reply(200, {'SigninToken': 'signin-token'})
                if self.headers.get('x-amz-sso_bearer_token') != portal.token:
                    return self.reply(401, {'message': 'Session token not found or invalid'})
                if url.path == '/assignment/accounts':
                    # The page token has characters that must be encoded in a URL.
                    if query.get('next_token') == 'page/2+=':
                        return self.reply(200, {'accountList': portal.accounts[2:]})
                    return self.reply(200, {'accountList': portal.accounts[:2], 'nextToken': 'page/2+='})
                if url.path == '/assignment/roles':
                    roles = ROLES.get(query['account_id'], [])
                    return self.reply(200, {'roleList': [{'roleName': role, 'accountId': query['account_id']} for role in roles]})
                self.reply(404, {})
            def reply(self, status, body):
                data = json.dumps(body).encode()
                self.send_response(status)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f'http://127.0.0.1:{self.server.server_address[1]}'


def setup(f, portal, logged_in=True):
    f.config.write_text(OWN)
    f.env['AWSHOP_SSO_PORTAL'] = portal.url
    if logged_in:
        cache = f.root/'.aws/sso/cache'
        cache.mkdir(parents=True)
        (cache/(hashlib.sha1(b'corp').hexdigest()+'.json')).write_text(json.dumps({'accessToken': 'test-token'}))


def sync(f):
    return subprocess.run([str(BINARY), 'sync'], env=f.env, text=True, capture_output=True)


def test_sync_adds_what_own_profiles_do_not_cover(f):
    portal = Portal()
    setup(f, portal)
    result = sync(f)
    assert result.returncode == 0, result.stderr
    # dev covers Dev/Admin; prod is taken by an own profile, so Prod gets its account ID.
    assert result.stdout.splitlines() == ['Added prod-333333333333', 'Added team-data-production-admin',
        'Added team-data-production-readonly'], result.stdout
    text = f.config.read_text()
    assert text.startswith(OWN.rstrip()+'\n\n# BEGIN awshop'), text
    assert '[profile team-data-production-readonly]\nsso_session = corp\nsso_account_id = 222222222222\nsso_role_name = ReadOnly\nregion = eu-north-1' in text
    assert text.count('[profile') == 5
    assert ('/assignment/accounts', {'max_result': '100', 'next_token': 'page/2+='}) in portal.requests
    # A second run changes nothing, and the file stays the same byte for byte.
    assert sync(f).stdout == 'Profiles are up to date with SSO.\n'
    assert f.config.read_text() == text
    # Lost access removes the account's profile; text after the block is kept.
    portal.accounts = ACCOUNTS[:2]
    f.config.write_text(text+'# after the block\n')
    assert sync(f).stdout == 'Removed prod-333333333333\n'
    assert f.config.read_text().endswith('# END awshop\n# after the block\n')


def test_own_profile_replaces_a_generated_one(f):
    portal = Portal()
    setup(f, portal)
    sync(f)
    text = f.config.read_text()
    f.config.write_text(text.replace('# BEGIN', '[profile mine]\nsso_session = corp\nsso_account_id = 333333333333\nsso_role_name = Admin\n\n# BEGIN'))
    assert sync(f).stdout == 'Removed prod-333333333333\n'


def test_without_a_valid_login_nothing_changes(f):
    portal = Portal()
    setup(f, portal, logged_in=False)
    result = sync(f)
    assert result.returncode == 1 and 'No valid SSO login' in result.stderr
    setup_cache = f.root/'.aws/sso/cache'
    setup_cache.mkdir(parents=True)
    (setup_cache/(hashlib.sha1(b'corp').hexdigest()+'.json')).write_text(json.dumps({'accessToken': 'expired'}))
    result = sync(f)
    assert result.returncode == 1 and 'No valid SSO login' in result.stderr
    assert f.config.read_text() == OWN


def test_sessions_sync_on_their_own(f):
    portal = Portal()
    setup(f, portal)
    other = """[sso-session other]
sso_start_url = https://other.awsapps.com/start/
sso_region = eu-west-1

# BEGIN awshop: profiles from SSO, kept up to date by awshop. Edits here are replaced.

# Other Org
[profile other-org]
sso_session = other
sso_account_id = 999999999999
sso_role_name = Admin
region = eu-west-1

# END awshop
"""
    f.config.write_text(OWN+'\n'+other)
    # Only corp is logged in: it syncs, and other keeps its profile as it was.
    result = sync(f)
    assert result.returncode == 0, result.stderr
    assert 'Removed' not in result.stdout and 'Added team-data-production-admin' in result.stdout
    text = f.config.read_text()
    assert '# Other Org\n[profile other-org]\nsso_session = other\nsso_account_id = 999999999999' in text
    assert text.count('# BEGIN awshop') == 1


def test_symlinked_config_stays_a_symlink(f):
    portal = Portal()
    setup(f, portal)
    target = f.root/'dotfiles-aws-config'
    f.config.rename(target)
    f.config.symlink_to(target)
    target.chmod(0o640)
    assert sync(f).returncode == 0
    assert f.config.is_symlink() and '# BEGIN awshop' in target.read_text()
    assert target.stat().st_mode & 0o777 == 0o640


def test_selector_shows_new_accounts_while_open(f):
    # Slow enough that the search is typed before the sync is done.
    portal = Portal(delay=1.0)
    setup(f, portal)
    s = Session(f, command=[str(BINARY), '--no-login'])
    s.wait('AWS profile selector', 'dev *')
    s.key(b'/team', 'No profiles match')
    # The sync finishes in the background and the list is rebuilt, with the search kept.
    s.wait('Search: team', 'team-data-production-admin', 'team-data-production-readonly')
    s.finish(b'\x1b')
    assert '# BEGIN awshop' in f.config.read_text()


def test_empty_config_says_it_is_syncing(f):
    portal = Portal(delay=0.5)
    setup(f, portal)
    f.config.write_text('[sso-session corp]\nsso_start_url = https://corp.awsapps.com/start/\nsso_region = eu-north-1\n')
    s = Session(f, command=[str(BINARY), '--no-login'])
    s.wait('Syncing accounts from SSO…')
    s.wait('dev', 'prod', 'team-data-production-admin')
    s.finish(b'\x1b')


def test_empty_config_without_login_says_how_to_log_in(f):
    portal = Portal()
    setup(f, portal, logged_in=False)
    f.config.write_text('[sso-session corp]\nsso_start_url = https://corp.awsapps.com/start/\nsso_region = eu-north-1\n')
    s = Session(f, command=[str(BINARY), '--no-login'])
    s.wait('No profiles yet. Log in with aws sso login --sso-session corp, then run awshop sync.')
    s.finish(b'\x1b')


def test_console_signs_in_without_the_token_on_a_command_line(f):
    portal = Portal()
    f.env['AWSHOP_SIGNIN'] = portal.url
    result = subprocess.run([str(BINARY), 'console', 'prod'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 0 and 'Opened the AWS console for prod in your browser.' in result.stderr, result.stderr
    # The credentials went to the sign-in endpoint, and the browser got a private page.
    [(path, query)] = portal.requests
    assert path == '/federation' and query['Action'] == 'getSigninToken'
    assert json.loads(query['Session']) == {'sessionId': 'ASIAEXAMPLE', 'sessionKey': 'secret-key', 'sessionToken': 'session-token'}
    opened = json.loads((f.root/'opened.json').read_text())
    assert opened['args'] == [str(f.root/'run/awshop/console.html')] and opened['mode'] == 0o600
    login = opened['page'].split('url=', 1)[1].split('"', 1)[0].replace('&amp;', '&')
    assert login == (portal.url+'/federation?Action=login&Issuer=awshop'
        '&Destination=https%3A%2F%2Fus%2Deast%2D1%2Econsole%2Eaws%2Eamazon%2Ecom%2Fconsole%2Fhome%3Fregion%3Dus%2Deast%2D1'
        '&SigninToken=signin%2Dtoken')
    # No program saw the token on its command line.
    assert 'signin-token' not in (f.root/'calls.jsonl').read_text()


def test_console_uses_the_browser_variable(f):
    portal = Portal()
    f.env['AWSHOP_SIGNIN'] = portal.url
    (f.bin/'browser').write_text((f.bin/'aws').read_text())
    (f.bin/'browser').chmod(0o755)
    f.env['BROWSER'] = 'browser --new-window'
    result = subprocess.run([str(BINARY), 'console', 'prod'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 0, result.stderr
    calls = [call for call in f.calls() if call['tool'] in ('browser', 'xdg-open')]
    assert calls == [{'tool': 'browser', 'args': ['--new-window', str(f.root/'run/awshop/console.html')]}]


def test_console_without_a_browser_says_where_the_page_is(f):
    portal = Portal()
    f.env['AWSHOP_SIGNIN'] = portal.url
    (f.bin/'xdg-open').unlink()
    result = subprocess.run([str(BINARY), 'console', 'prod'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 1
    assert 'Could not open a browser with xdg-open' in result.stderr and 'xdg-mime query default text/html' in result.stderr
    assert str(f.root/'run/awshop/console.html') in result.stderr


def test_console_reports_credential_errors(f):
    (f.root/'fail-export').touch()
    result = subprocess.run([str(BINARY), 'console', 'prod'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 1 and "Could not get credentials for 'prod'" in result.stderr
    assert 'Token has expired' in result.stderr and not (f.root/'opened.json').exists()


def test_console_needs_temporary_credentials(f):
    (f.root/'static-keys').touch()
    result = subprocess.run([str(BINARY), 'console', 'prod'], env=f.env, text=True, capture_output=True)
    assert result.returncode == 1 and 'needs temporary credentials' in result.stderr
    assert not (f.root/'opened.json').exists()


def test_console_from_the_profile_menu(f):
    portal = Portal()
    f.env['AWSHOP_SIGNIN'] = portal.url
    zsh = shutil.which('zsh')
    if not zsh:
        print('SKIP: zsh is not installed')
        return
    command = [zsh, '-f', '-c', f'source {ROOT}/completions/awshop.zsh; awshop --no-login; print -r -- "PROFILE=${{AWS_PROFILE}}"']
    s = Session(f, command=command)
    s.wait('AWS profile selector', 'dev *')
    s.key(b'\r', '› Use in this shell', 'AWS console', 'Sign in to the console in your browser')
    s.key(b'\x1b[B', '› AWS console')
    # Opening the console leaves the shell's profile as it was.
    assert s.finish(b'\r') == 'PROFILE=dev\n'
    assert 'Opened the AWS console for dev in your browser.' in ANSI.sub(b'', s.output).decode()
    assert 'region%3Deu%2Dnorth%2D1' in json.loads((f.root/'opened.json').read_text())['page']


def test_sync_says_what_is_missing(f):
    f.config.unlink()
    result = sync(f)
    assert result.returncode == 1 and f'No AWS config at {f.config}.' in result.stderr
    f.config.write_text('[profile prod]\nregion = us-east-1\n')
    result = sync(f)
    assert result.returncode == 1 and 'No [sso-session] in' in result.stderr


def test_sync_retries_passing_portal_errors(f):
    portal = Portal(failures=2)
    setup(f, portal)
    result = sync(f)
    assert result.returncode == 0 and 'Added prod-333333333333' in result.stdout, result.stderr
    # A lasting error changes nothing.
    portal.status = 500
    text = f.config.read_text()
    result = sync(f)
    assert result.returncode == 1 and 'SSO portal request failed' in result.stderr
    assert f.config.read_text() == text


def test_sync_keeps_edits_made_while_it_runs(f):
    portal = Portal(delay=0.4)
    setup(f, portal)
    running = subprocess.Popen([str(BINARY), 'sync'], env=f.env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    time.sleep(0.2)
    with f.config.open('a') as config:
        config.write('\n# added while awshop listed the accounts\n')
    stdout, stderr = running.communicate(timeout=20)
    assert running.returncode == 0, stderr
    text = f.config.read_text()
    assert '# added while awshop listed the accounts' in text and '[profile team-data-production-admin]' in text


def test_sync_can_be_turned_off(f):
    portal = Portal()
    setup(f, portal)
    f.awshop_config('[sso]\nsync = false\n')
    s = Session(f, command=[str(BINARY), '--no-login'])
    s.wait('AWS profile selector', 'dev *')
    time.sleep(1)
    s.finish(b'\x1b')
    assert not portal.requests and f.config.read_text() == OWN


def test_selector_details_can_be_chosen(f):
    setup(f, Portal())
    f.awshop_config('[sso]\nsync = false\n')
    s = Session(f, command=[str(BINARY), '--no-login'])
    s.wait('Profile  Login     Account       Role   Region', 'dev *    SSO corp  111111111111  Admin  eu-north-1')
    # The search matches the account ID.
    s.key(b'/1111', 'Search: 1111', '› dev')
    s.finish(b'\x1b')

    f.awshop_config('[sso]\nsync = false\n\n[selector]\ndetails = ["region", "role"]\n')
    s = Session(f, command=[str(BINARY), '--no-login'])
    s.wait('Profile  Region      Role', 'dev *    eu-north-1  Admin')
    # The account ID is searched even when it isn't shown; other hidden fields aren't.
    s.key(b'/1111', 'Search: 1111', '› dev')
    s.key(b'\x7f'*4+b'corp', 'No profiles match')
    s.finish(b'\x1b')


if __name__ == '__main__':
    tests = [value for name, value in list(globals().items()) if name.startswith('test_')]
    for test in tests:
        with tempfile.TemporaryDirectory(prefix='awshop-sso-test-') as directory:
            test(Fixture(directory))
        print('PASS:', test.__name__)

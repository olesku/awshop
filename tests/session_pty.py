#!/usr/bin/env python3
"""Session lifecycle and status-panel checks. No AWS calls or real config writes."""
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import tempfile
import time
from eks_pty import ANSI, BINARY, ROOT, Fixture, Session


SHELLS = {
    'zsh': ['/usr/bin/zsh', '-f', '-c'],
    'bash': ['/usr/bin/bash', '--norc', '--noprofile', '-c'],
    'fish': ['/usr/bin/fish', '--no-config', '-c'],
}
CURRENT = 'zsh'


class Skip(Exception):
    pass


def shell(f, body, fish=None):
    # Load the integration the way an rc file does. `body` uses syntax that zsh and bash share;
    # fish needs its own. fish's read draws its line editor on stdout, so fish bodies wait with head.
    if CURRENT == 'fish':
        return Session(f, command=[*SHELLS['fish'], 'command awshop init fish | source; '+fish])
    return Session(f, command=[*SHELLS[CURRENT], 'eval "$(command awshop init '+CURRENT+')"; '+body])


def choose(s):
    s.wait('› dev-1', '^L reload')
    s.key(b'\r', 'Start EKS session', 'This shell (temporary)', 'Create a temporary')


def test_roundtrip(f):
    original = f.root/'kubeconfig'
    original.write_text('{"keep": "original"}')
    s = shell(f, '''
        original=$KUBECONFIG
        awshop eks --no-login || exit 2
        session_file=$KUBECONFIG
        [[ -f "$session_file" && "$session_file" != "$original" ]] || exit 3
        printf '%s\\n' "SESSION=$session_file"
        awshop eks leave || exit 4
        [[ "$KUBECONFIG" == "$original" && ! -e "$session_file" ]] || exit 5
        printf '%s\\n' restored
    ''', fish='''
        set original $KUBECONFIG
        awshop eks --no-login; or exit 2
        set session_file $KUBECONFIG
        test -f "$session_file" -a "$session_file" != "$original"; or exit 3
        printf '%s\\n' "SESSION=$session_file"
        awshop eks leave; or exit 4
        test "$KUBECONFIG" = "$original" -a ! -e "$session_file"; or exit 5
        printf '%s\\n' restored
    ''')
    choose(s)
    output = s.finish(b'\r')
    assert output.endswith('restored\n') and 'SESSION=' in output
    assert original.read_text() == '{"keep": "original"}'
    assert not f.sessions()
    settings = json.loads(f.preferences().read_text())['plugins']['eks']['dev']
    assert next(iter(settings.values()))['kubeconfig'] == str(original)


def test_unset_and_empty_restore(f):
    for initial, fish, expected_set in [('unset KUBECONFIG', 'set -e KUBECONFIG', ''),
            ('export KUBECONFIG=""', 'set -gx KUBECONFIG ""', '1')]:
        s = shell(f, initial+''';
            awshop eks --no-login || exit 2
            session_file=$KUBECONFIG
            awshop eks leave || exit 3
            printf '%s\\n' "SET=${KUBECONFIG+1} VALUE=$KUBECONFIG"
            [[ ! -e "$session_file" ]] || exit 4
        ''', fish=fish+'''
            awshop eks --no-login; or exit 2
            set session_file $KUBECONFIG
            awshop eks leave; or exit 3
            set -q KUBECONFIG; and set is_set 1; or set is_set ''
            printf '%s\\n' "SET=$is_set VALUE=$KUBECONFIG"
            test ! -e "$session_file"; or exit 4
        ''')
        choose(s)
        assert s.finish(b'\r') == 'SET='+expected_set+' VALUE=\n'


def test_replacement_preserves_original(f):
    s = shell(f, '''
        original=$KUBECONFIG
        awshop eks --no-login || exit 2
        first=$KUBECONFIG
        awshop eks --no-login || exit 3
        second=$KUBECONFIG
        [[ "$first" != "$second" && ! -e "$first" && -f "$second" ]] || exit 4
        awshop eks leave || exit 5
        [[ "$KUBECONFIG" == "$original" && ! -e "$second" ]] || exit 6
        printf '%s\\n' replaced
    ''', fish='''
        set original $KUBECONFIG
        awshop eks --no-login; or exit 2
        set first $KUBECONFIG
        awshop eks --no-login; or exit 3
        set second $KUBECONFIG
        test "$first" != "$second" -a ! -e "$first" -a -f "$second"; or exit 4
        awshop eks leave; or exit 5
        test "$KUBECONFIG" = "$original" -a ! -e "$second"; or exit 6
        printf '%s\\n' replaced
    ''')
    choose(s)
    s.key(b'\r', 'dev-1', 'dev-1 *')
    s.key(b'\r', 'Start EKS session')
    assert s.finish(b'\r') == 'replaced\n'
    assert not f.sessions()


def test_failed_replacement_keeps_existing_session(f):
    s = shell(f, '''
        awshop eks --no-login || exit 2
        first=$KUBECONFIG
        : > "$AWSHOP_TEST_ROOT/fail-update"
        awshop eks --no-login
        [[ "$KUBECONFIG" == "$first" && -f "$first" ]] || exit 3
        awshop eks leave || exit 4
        printf '%s\\n' kept
    ''', fish='''
        awshop eks --no-login; or exit 2
        set first $KUBECONFIG
        printf '' > "$AWSHOP_TEST_ROOT/fail-update"
        awshop eks --no-login
        test "$KUBECONFIG" = "$first" -a -f "$first"; or exit 3
        awshop eks leave; or exit 4
        printf '%s\\n' kept
    ''')
    choose(s)
    s.key(b'\r', '› dev-1', 'dev-1 *', '^L reload')
    s.key(b'\r', 'Start EKS session')
    s.key(b'\r', 'Setup failed', 'AccessDeniedException')
    assert s.finish(b'\x03') == 'kept\n'
    assert not f.sessions()


def test_saved_config_exits_session_without_writing_temp_path(f):
    s = shell(f, '''
        original=$KUBECONFIG
        awshop eks --no-login || exit 2
        temporary=$KUBECONFIG
        awshop eks --no-login --saved || exit 3
        [[ "$KUBECONFIG" == "$original" && ! -e "$temporary" && -f "$original" ]] || exit 4
        printf '%s\\n' saved
    ''', fish='''
        set original $KUBECONFIG
        awshop eks --no-login; or exit 2
        set temporary $KUBECONFIG
        awshop eks --no-login --saved; or exit 3
        test "$KUBECONFIG" = "$original" -a ! -e "$temporary" -a -f "$original"; or exit 4
        printf '%s\\n' saved
    ''')
    choose(s)
    s.key(b'\r', '› dev-1', 'dev-1 *', '^L reload')
    s.key(b'\r', 'Configure and activate', 'Saved config')
    assert s.finish(b'\r') == 'saved\n'
    args = f.updates()[-1]
    assert args[args.index('--kubeconfig')+1] == str(f.root/'kubeconfig')


def test_shell_exit_cleans_up_and_permissions_are_private(f):
    s = shell(f, '''
        awshop eks --no-login || exit 2
        printf '%s\\n' "$KUBECONFIG"
        read -r unused
    ''', fish='''
        awshop eks --no-login; or exit 2
        printf '%s\\n' "$KUBECONFIG"
        /usr/bin/head -n1 >/dev/null
    ''')
    choose(s)
    s.key(b'\r', 'Session active in this shell')
    [directory] = f.sessions()
    config = directory/'config'
    assert config.parent.parent == f.root/'run/awshop/sessions'
    for path in [config, config.parent/'session.json']:
        assert path.stat().st_mode & 0o777 == 0o600
    for path in [config.parent, config.parent.parent, config.parent.parent.parent]:
        assert path.stat().st_mode & 0o777 == 0o700
    assert s.finish(b'\n') == str(config)+'\n'
    assert not config.parent.exists()


def test_two_shells_are_isolated(f):
    first = shell(f, 'awshop eks --no-login; read -r unused; awshop eks leave',
        fish='awshop eks --no-login; /usr/bin/head -n1 >/dev/null; awshop eks leave')
    choose(first)
    first.key(b'\r', 'Session active in this shell')
    [first_dir] = f.sessions()
    first_file = first_dir/'config'
    second = shell(f, 'awshop eks --no-login; read -r unused; awshop eks leave',
        fish='awshop eks --no-login; /usr/bin/head -n1 >/dev/null; awshop eks leave')
    choose(second)
    second.key(b'\r', 'Session active in this shell')
    [second_file] = [path/'config' for path in f.sessions() if path != first_dir]
    assert first_file != second_file and first_file.exists() and second_file.exists()
    second.finish(b'\n')
    assert not second_file.exists() and first_file.exists()
    first.finish(b'\n')
    assert not first_file.exists()


def test_subshell_cannot_leave_parent_session(f):
    s = shell(f, '''
        awshop eks --no-login || exit 2
        original=$KUBECONFIG
        ( awshop eks leave )
        [[ -f "$original" && "$KUBECONFIG" == "$original" ]] || exit 3
        awshop eks leave
        printf '%s\\n' parent-kept
    ''', fish='''
        awshop eks --no-login; or exit 2
        set original $KUBECONFIG
        /usr/bin/fish --no-config -c 'command awshop init fish | source; awshop eks leave'
        test -f "$original" -a "$KUBECONFIG" = "$original"; or exit 3
        awshop eks leave
        printf '%s\\n' parent-kept
    ''')
    choose(s)
    assert s.finish(b'\r') == 'parent-kept\n'


def test_nested_shell_saves_to_the_original_file(f):
    child = shlex.join([*SHELLS[CURRENT], 'eval "$(command awshop init '+CURRENT+')"; awshop eks --no-login --saved tools-1'])
    fish_child = shlex.join([*SHELLS['fish'], 'command awshop init fish | source; awshop eks --no-login --saved tools-1'])
    s = shell(f, '''
        awshop eks --no-login || exit 2
        '''+child+''' || exit 3
        printf '%s\\n' nested
    ''', fish='''
        awshop eks --no-login; or exit 2
        '''+fish_child+'''; or exit 3
        printf '%s\\n' nested
    ''')
    choose(s)
    s.key(b'\r', '› tools-1')
    s.key(b'\r', 'Configure and activate', str(f.root/'kubeconfig'))
    assert s.finish(b'\r') == 'nested\n'
    args = f.updates()[-1]
    assert args[args.index('--kubeconfig')+1] == str(f.root/'kubeconfig')


def test_session_opens_k9s(f):
    k9s = f.k9s()
    s = shell(f, '''
        awshop eks --no-login || exit 2
        printf '%s\\n' "$KUBECONFIG"
    ''', fish='''
        awshop eks --no-login; or exit 2
        printf '%s\\n' "$KUBECONFIG"
    ''')
    choose(s)
    s.key(b'\x1b[B', '› Start EKS session and open k9s')
    session_file = s.finish(b'\r').strip()
    assert session_file.endswith('/config') and '/awshop/sessions/eks-' in session_file
    assert json.loads(k9s.read_text()) == {'args':['--context', 'dev/eu-north-1/dev-1'],
        'kubeconfig':session_file, 'tty':True}
    assert 'Session active in this shell' in s.output.decode()


def test_hostile_path_is_quoted(f):
    # Plugins refuse control characters in paths, so the newline is in the previous value.
    special = f.root/"run ' $(touch pwned) dir"
    special.mkdir(mode=0o700)
    f.env['XDG_RUNTIME_DIR'] = str(special)
    f.env['KUBECONFIG'] = str(f.root/"saved ' $(touch pwned) `touch pwned` \\ \n config")
    s = shell(f, '''
        original=$KUBECONFIG
        awshop eks --no-login || exit 2
        [[ -f "$KUBECONFIG" ]] || exit 3
        awshop eks leave || exit 4
        [[ "$KUBECONFIG" == "$original" ]] || exit 5
        printf '%s\\n' quoted
    ''', fish='''
        set original $KUBECONFIG
        awshop eks --no-login; or exit 2
        test -f "$KUBECONFIG"; or exit 3
        awshop eks leave; or exit 4
        test "$KUBECONFIG" = "$original"; or exit 5
        printf '%s\\n' quoted
    ''')
    choose(s)
    assert s.finish(b'\r') == 'quoted\n'
    assert not (Path.cwd()/'pwned').exists() and not (f.root/'pwned').exists()
    assert not list((special/'awshop/sessions').iterdir())


def test_direct_invocation_does_not_create_a_session(f):
    s = Session(f, command=[str(BINARY), 'eks', '--no-login'])
    choose(s)
    s.key(b'\r', 'Shell integration required')
    s.finish(b'\x03')
    assert not f.updates() and not f.sessions()


def test_status_tracks_selection(f):
    s = Session(f, command=[str(BINARY), 'eks', '--no-login'])
    choose(s)
    s.key(b'\x1b[B', 'Choose between a temporary session')
    s.key(b'\x1b[B', 'Choose which IAM role kubectl uses')
    s.key(b'\x1b[B'*2, '› Change lookup role', 'Choose a role for retrieving cluster information')
    s.finish(b'\x03')


def test_scope_menu_and_default_after_saved_setup(f):
    s = Session(f, command=[str(BINARY), 'eks', '--no-login'])
    choose(s)
    s.key(b'\x1b[B\r', '› This shell', 'Saved config')
    s.key(b'\x1b[B\r', '› Configure and activate', 'Saved config')
    assert s.finish(b'\r') == ''
    assert (f.root/'kubeconfig').exists()
    s = Session(f, command=[str(BINARY), 'eks', '--no-login'])
    choose(s)
    s.finish(b'\x03')
    assert len(f.updates()) == 1


def test_manual_environment_change_is_preserved(f):
    s = shell(f, '''
        awshop eks --no-login || exit 2
        session_file=$KUBECONFIG
        export KUBECONFIG=manually-selected-config
        awshop eks leave || exit 3
        [[ "$KUBECONFIG" == manually-selected-config && ! -e "$session_file" ]] || exit 4
        printf '%s\\n' manual-kept
    ''', fish='''
        awshop eks --no-login; or exit 2
        set session_file $KUBECONFIG
        set -gx KUBECONFIG manually-selected-config
        awshop eks leave; or exit 3
        test "$KUBECONFIG" = manually-selected-config -a ! -e "$session_file"; or exit 4
        printf '%s\\n' manual-kept
    ''')
    choose(s)
    assert s.finish(b'\r') == 'manual-kept\n'


def test_readonly_environment_is_reported(f):
    if CURRENT == 'fish':
        raise Skip('fish has no readonly variables')
    s = shell(f, '''
        original=$KUBECONFIG
        readonly KUBECONFIG
        if awshop eks --no-login; then exit 2; fi
        [[ "$KUBECONFIG" == "$original" ]] || exit 3
        printf '%s\\n' readonly-kept
    ''')
    choose(s)
    assert s.finish(b'\r') == 'readonly-kept\n'
    output = ANSI.sub(b'', s.output).decode()
    # zsh says "read-only variable", Bash "readonly variable".
    assert ('read-only variable' in output or 'readonly variable' in output) and 'Session active' not in output
    assert not f.sessions()


def test_selector_sets_profile_through_shell_function(f):
    s = shell(f, '''
        awshop --no-login prod || exit 2
        printf '%s\\n' "PROFILE=$AWS_PROFILE"
    ''', fish='''
        awshop --no-login prod; or exit 2
        printf '%s\\n' "PROFILE=$AWS_PROFILE"
    ''')
    s.wait('› prod')
    # Enter opens the profile's menu, which starts at using it; Enter again does that.
    s.key(b'\r', 'awshop › prod', '› Use in this shell', 'AWS_PROFILE=prod', 'EKS', 'SSM')
    assert s.finish(b'\r') == 'PROFILE=prod\n'
    assert 'Set environment to prod' in s.output.decode()


def test_selector_opens_a_plugin_for_the_profile(f):
    s = shell(f, '''
        awshop --no-login || exit 2
        printf '%s\\n' "PROFILE=$AWS_PROFILE"
        [[ "$KUBECONFIG" == */awshop/sessions/*/config ]] && printf '%s\\n' session
    ''', fish='''
        awshop --no-login; or exit 2
        printf '%s\\n' "PROFILE=$AWS_PROFILE"
        string match -q '*/awshop/sessions/*/config' -- $KUBECONFIG; and printf '%s\\n' session
    ''')
    s.wait('AWS profile selector')
    assert '→' not in ANSI.sub(b'', s.latest).decode().splitlines()[-1]
    s.key(b'prod', '› prod')
    s.key(b'\r', 'awshop › prod', '› Use in this shell', 'EKS', 'SSM')
    s.key(b'\x1b[B'*2, '› EKS')
    s.key(b'\r', '› dev-1', 'AWS profile  prod')
    # ← and Escape go back one level: to the menu, where EKS stays highlighted, then to
    # the profiles as left.
    s.key(b'\x1b[D', 'awshop › prod', '› EKS')
    s.key(b'\x1b[D', 'Search: prod', '› prod')
    # → does nothing here: Enter opens the menu, which starts at using the profile.
    s.key(b'\x1b[C')
    s.key(b'\r', '› Use in this shell')
    s.key(b'\x1b[B'*2+b'\r', '› dev-1', '←/esc back')
    s.key(b'\r', 'Start EKS session')
    assert s.finish(b'\r') == 'PROFILE=prod\nsession\n'
    output = ANSI.sub(b'', s.output).decode()
    assert 'Set environment to prod' in output and 'Session active in this shell' in output
    args = f.updates()[-1]
    assert args[args.index('--profile')+1] == 'prod'
    # One alternate screen for the whole flow: the shell never shows in between.
    assert s.output.count(b'\x1b[?1049h') == 1 and s.output.count(b'\x1b[?1049l') == 1


def test_export_flag_can_be_evaluated(f):
    s = shell(f, '''
        eval "$(command awshop --no-login --export prod)" || exit 2
        printf '%s\\n' "PROFILE=$AWS_PROFILE"
        awshop --no-login --export prod
    ''', fish='''
        eval (command awshop --no-login --export prod); or exit 2
        printf '%s\\n' "PROFILE=$AWS_PROFILE"
        awshop --no-login --export prod
    ''')
    s.wait('› prod')
    s.key(b'\r', '› prod')
    assert s.finish(b'\r') == 'PROFILE=prod\nexport AWS_PROFILE=prod\n'


def test_direct_terminal_run_prints_export_command(f):
    f.env['SHELL'] = SHELLS[CURRENT][0]
    s = Session(f, command=['/bin/sh', '-c', 'exec awshop --no-login prod >/dev/tty'])
    s.wait('› prod')
    assert s.finish(b'\r') == ''
    output = ANSI.sub(b'', s.output).decode()
    assert 'export AWS_PROFILE=prod' in output
    hint = {'fish': 'awshop init fish | source to ~/.config/fish/config.fish'}.get(
        CURRENT, 'eval "$(awshop init '+CURRENT+')" to ~/.'+CURRENT+'rc')
    assert hint in output


def test_prompt_segments(f):
    s = shell(f, '''
        printf '%s\\n' "$(awshop_prompt)"
        AWSHOP_PROMPT_SEGMENTS="aws region eks"
        AWS_REGION=eu-north-1
        saved=$KUBECONFIG
        KUBECONFIG="$AWSHOP_TEST_ROOT/prompt-kubeconfig"
        printf 'kind: Config\\ncurrent-context: "dev/eu-north-1/tools-1"\\n' > "$KUBECONFIG"
        printf '%s\\n' "$(awshop_prompt)"
        AWSHOP_PROMPT_EKS=full
        printf '%s\\n' "$(awshop_prompt)"
        AWSHOP_PROMPT_EKS=short
        KUBECONFIG="$AWSHOP_TEST_ROOT/missing:$KUBECONFIG"
        printf '%s\\n' "$(awshop_prompt)"
        KUBECONFIG=$saved
        awshop eks --no-login || exit 2
        printf '%s\\n' "$(awshop_prompt)"
        ( printf '%s\\n' "subshell $(awshop_prompt)" )
        awshop eks leave || exit 3
        AWSHOP_PROMPT_SEGMENTS="region"
        printf '%s\\n' "$(awshop_prompt)"
        unset KUBECONFIG AWS_REGION
        AWSHOP_PROMPT_SEGMENTS="aws region eks"
        printf '%s\\n' "$(awshop_prompt)"
        unset AWS_PROFILE AWS_DEFAULT_PROFILE
        AWS_REGION=eu-north-1
        KUBECONFIG="$AWSHOP_TEST_ROOT/prompt-kubeconfig"
        printf 'EMPTY=%s\\n' "$(awshop_prompt)"
        AWS_DEFAULT_PROFILE=fallback
        printf '%s\\n' "$(awshop_prompt)"
    ''', fish='''
        printf '%s\\n' (awshop_prompt)
        set -gx AWSHOP_PROMPT_SEGMENTS "aws region eks"
        set -gx AWS_REGION eu-north-1
        set saved $KUBECONFIG
        set KUBECONFIG "$AWSHOP_TEST_ROOT/prompt-kubeconfig"
        printf 'kind: Config\\ncurrent-context: "dev/eu-north-1/tools-1"\\n' > $KUBECONFIG
        printf '%s\\n' (awshop_prompt)
        set AWSHOP_PROMPT_EKS full
        printf '%s\\n' (awshop_prompt)
        set AWSHOP_PROMPT_EKS short
        set KUBECONFIG "$AWSHOP_TEST_ROOT/missing:$KUBECONFIG"
        printf '%s\\n' (awshop_prompt)
        set KUBECONFIG $saved
        awshop eks --no-login; or exit 2
        printf '%s\\n' (awshop_prompt)
        printf '%s\\n' "subshell "(/usr/bin/fish --no-config -c 'command awshop init fish | source; awshop_prompt')
        awshop eks leave; or exit 3
        set AWSHOP_PROMPT_SEGMENTS region
        printf '%s\\n' (awshop_prompt)
        set -e KUBECONFIG AWS_REGION
        set AWSHOP_PROMPT_SEGMENTS "aws region eks"
        printf '%s\\n' (awshop_prompt)
        set -e AWS_PROFILE AWS_DEFAULT_PROFILE
        set -gx AWS_REGION eu-north-1
        set -gx KUBECONFIG "$AWSHOP_TEST_ROOT/prompt-kubeconfig"
        printf 'EMPTY=%s\\n' (awshop_prompt)
        set -gx AWS_DEFAULT_PROFILE fallback
        printf '%s\\n' (awshop_prompt)
    ''')
    choose(s)
    lines = s.finish(b"\r").splitlines()
    assert lines == [
        'aws:dev',
        'aws:dev@eu-north-1 eks:tools-1',
        'aws:dev@eu-north-1 eks:dev/eu-north-1/tools-1',
        'aws:dev@eu-north-1 eks:tools-1',
        'aws:dev@eu-north-1 eks:dev-1*',
        'subshell aws:dev@eu-north-1 eks:dev-1*',
        'region:eu-north-1',
        'aws:dev',
        'EMPTY=',
        'aws:fallback@eu-north-1 eks:tools-1',
    ], lines


def test_init_prints_the_integration_and_keeps_the_rc_file_running(f):
    result = subprocess.run([str(BINARY), 'init', CURRENT], env=f.env, capture_output=True)
    assert result.returncode == 0
    assert result.stdout == (ROOT/f'completions/awshop.{CURRENT}').read_bytes()
    rc = f.root/'rc'
    rc.write_text({'fish': 'command awshop init fish | source\nprintf "%s\\n" (awshop_prompt)\n'}.get(
        CURRENT, 'eval "$(command awshop init '+CURRENT+')"\nprintf "%s\\n" "$(awshop_prompt)"\n'))
    loaded = subprocess.run([*SHELLS[CURRENT][:-1], '-c', 'source '+shlex.quote(str(rc))+'; printf after-rc'],
        env=f.env, text=True, capture_output=True)
    assert loaded.stdout == 'aws:dev\nafter-rc', loaded.stderr
    unknown = subprocess.run([str(BINARY), 'init', 'tcsh'], env=f.env, text=True, capture_output=True)
    assert unknown.returncode == 1 and 'awshop init zsh|bash|fish' in unknown.stderr


def test_output_through_function_is_not_evaluated(f):
    # Evaluating the init script would define awshop_prompt again.
    s = shell(f, '''
        unset -f awshop_prompt
        awshop --help || exit 2
        awshop plugins || exit 3
        awshop eks --help || exit 4
        awshop init '''+CURRENT+''' || exit 5
        type awshop_prompt >/dev/null 2>&1 && exit 6
        printf '%s\\n' "PROFILE=$AWS_PROFILE"
    ''', fish='''
        functions -e awshop_prompt
        awshop --help; or exit 2
        awshop plugins; or exit 3
        awshop eks --help; or exit 4
        awshop init fish; or exit 5
        functions -q awshop_prompt; and exit 6
        printf '%s\\n' "PROFILE=$AWS_PROFILE"
    ''')
    output = s.finish()
    assert 'awshop add [profile-name]' in output and 'Temporary sessions require' in output
    assert (ROOT/f'completions/awshop.{CURRENT}').read_text() in output
    assert output.endswith('PROFILE=dev\n')


def test_stdout_redirects_through_function(f):
    s = shell(f, '''
        awshop init '''+CURRENT+''' > "$AWSHOP_TEST_ROOT/script" || exit 2
        awshop plugins | /usr/bin/grep -c eks
    ''', fish='''
        awshop init fish > "$AWSHOP_TEST_ROOT/script"; or exit 2
        awshop plugins | /usr/bin/grep -c eks
    ''')
    assert s.finish() == '1\n'
    assert (f.root/'script').read_bytes() == (ROOT/f'completions/awshop.{CURRENT}').read_bytes()


def test_killed_shell_session_is_swept(f):
    first = shell(f, 'awshop eks --no-login; read -r unused', fish='awshop eks --no-login; /usr/bin/head -n1 >/dev/null')
    choose(first)
    first.key(b'\r', 'Session active in this shell')
    [killed] = f.sessions()
    second = shell(f, '''
        awshop eks --no-login || exit 2
        read -r unused
        awshop eks --help >/dev/null
        read -r unused
    ''', fish='''
        awshop eks --no-login; or exit 2
        /usr/bin/head -n1 >/dev/null
        awshop eks --help >/dev/null
        /usr/bin/head -n1 >/dev/null
    ''')
    choose(second)
    second.key(b'\r', 'Session active in this shell')
    [kept] = [path for path in f.sessions() if path != killed]
    os.kill(first.pid, signal.SIGKILL)
    os.waitpid(first.pid, 0)
    assert killed.exists()
    second.key(b'\n')
    deadline = time.monotonic()+5
    while killed.exists() and time.monotonic() < deadline:
        time.sleep(.05)
    assert not killed.exists() and kept.exists()
    second.finish(b'\n')
    assert not f.sessions()


def test_overwritten_exit_trap_is_swept(f):
    if CURRENT != 'bash':
        raise Skip('only Bash has an EXIT trap that can be replaced')
    s = shell(f, "trap 'true' EXIT; awshop eks --no-login")
    choose(s)
    s.finish(b'\r')
    assert f.sessions()
    shell(f, 'awshop eks --help >/dev/null').finish()
    assert not f.sessions()


def test_outdated_wrapper_is_reported(f):
    for args in [['--shell', CURRENT, 'eks'], ['eks', '--no-login', '--shell', CURRENT]]:
        result = subprocess.run([str(BINARY), *args], env=f.env, text=True, capture_output=True)
        assert result.returncode == 1 and 'out of date' in result.stderr, result.stderr
    assert not f.sessions()


def test_code_channel_closed_for_children(f):
    f.config.write_text('[profile dev]\nregion=eu-north-1\nsso_session=my-sso\n[sso-session my-sso]\nsso_region=eu-north-1\n')
    (f.root/'login-needed').touch()
    (f.root/'slow-child').touch()
    s = shell(f, '''
        awshop dev || exit 2
        printf '%s\\n' "PROFILE=$AWS_PROFILE"
    ''', fish='''
        awshop dev; or exit 2
        printf '%s\\n' "PROFILE=$AWS_PROFILE"
    ''')
    s.wait('› dev')
    s.key(b'\r', '› Use in this shell')
    started = time.monotonic()
    assert s.finish(b'\r') == 'PROFILE=dev\n'
    assert time.monotonic()-started < 4


if __name__ == '__main__':
    tests = [value for name, value in list(globals().items()) if name.startswith('test_')]
    for name, command in SHELLS.items():
        if not Path(command[0]).exists():
            print(f'SKIP: {name} is not installed')
            continue
        CURRENT = name
        for test in tests:
            try:
                with tempfile.TemporaryDirectory(prefix='awshop-session-test-') as directory:
                    test(Fixture(directory))
            except Skip as reason:
                print(f'SKIP [{name}]: {test.__name__}: {reason}')
                continue
            print(f'PASS [{name}]:', test.__name__)

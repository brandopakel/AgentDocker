#!/usr/bin/env python3
"""Accept immutable hosted Windows previews in private installations on Windows.

Without --baseline-tag this proves first installation/default-feed discovery,
not a two-release upgrade. A baseline must itself support native installation;
portable-only releases are refused before execution. Service trials use private
Task Scheduler registrations and do not change the runner's normal installation.
Across a schema upgrade, acceptance requires rollback refusal with the active
installation unchanged; it never treats that as a successful database downgrade.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import uuid

from windows_package_smoke import extract_checked

REPO = 'brandopakel/AgentDocker'
TARGET = 'x86_64-pc-windows-msvc'
ARCHIVE = f'agentdocker-desktop-{TARGET}.zip'
PREVIEW_FEED = f'https://github.com/{REPO}/releases/download/channel-preview-windows/updates-preview-windows.json'


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(block)
    return result.hexdigest()


def identity(tag, source):
    if not re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+-[A-Za-z0-9.-]+', tag):
        raise ValueError('an explicit prerelease tag is required')
    if not re.fullmatch(r'[0-9a-f]{40}', source):
        raise ValueError('an exact expected source commit is required')


def gh(*args):
    return subprocess.check_output(['gh', *args], timeout=180)


def api(path):
    return json.loads(gh('api', f'repos/{REPO}/{path}'))


def stable():
    release = api('releases/latest')
    tap = json.loads(gh('api', 'repos/brandopakel/homebrew-tap/git/ref/heads/main'))
    return {'release_id': release['id'], 'tag': release['tag_name'],
            'assets': {a['name']: a['digest'] for a in release['assets']},
            'tap': tap['object']['sha']}


def validate_manifest(manifest, tag, source):
    identity(tag, source)
    if (manifest.get('source_commit') != source or manifest.get('source_dirty') is not False
            or manifest.get('version') != tag[1:] or manifest.get('target') != TARGET
            or type(manifest.get('state_schema')) is not int or manifest['state_schema'] <= 0
            or manifest.get('installation_lock') != 1 or manifest.get('launcher_redirect') != 2
            or set(manifest.get('artifacts', {})) != {ARCHIVE}):
        raise ValueError('hosted preview lacks the exact source/version/native installation contract')


def download(tag, source, destination):
    identity(tag, source)
    destination.mkdir()
    release = api(f'releases/tags/{tag}')
    if release['draft'] or not release['prerelease'] or release['tag_name'] != tag:
        raise ValueError('the selected release is not a published prerelease')
    ref = api(f'git/ref/tags/{tag}')['object']
    if ref['type'] == 'tag':
        ref = api(f"git/tags/{ref['sha']}")['object']
    if ref.get('type') != 'commit' or ref.get('sha') != source:
        raise ValueError('the immutable tag differs from the expected source')
    names = ['windows-preview-manifest.json', ARCHIVE, ARCHIVE + '.sha256']
    assets = {a['name']: a for a in release['assets']}
    for name in names:
        asset = assets[name]
        limit = 40 * 1024 ** 2 if name == ARCHIVE else 1024 ** 2
        if not 0 < asset['size'] <= limit:
            raise ValueError('hosted asset exceeds its size budget')
        gh('release', 'download', tag, '--repo', REPO, '--pattern', name, '--dir', str(destination))
        path = destination / name
        if path.stat().st_size != asset['size'] or asset['digest'] != 'sha256:' + digest(path):
            raise ValueError('download differs from the hosted asset digest/size')
    manifest = json.loads((destination / names[0]).read_text(encoding='utf-8'))
    validate_manifest(manifest, tag, source)
    words = (destination / (ARCHIVE + '.sha256')).read_text(encoding='utf-8').split()
    if words != [digest(destination / ARCHIVE), ARCHIVE]:
        raise ValueError('published archive checksum differs')
    app = extract_checked(destination / ARCHIVE, destination / 'extracted', manifest)
    (destination / 'release.json').write_text(json.dumps(release, indent=2) + '\n', encoding='utf-8')
    return app, manifest


def validate_update(update, manifest, expected_upgrade):
    if (update.get('feed') != PREVIEW_FEED or update.get('target') != TARGET
            or update.get('channel') != 'preview' or update.get('update_available') is not expected_upgrade
            or update.get('preview_consent_required') is not False
            or update.get('available', {}).get('source_commit') != manifest['source_commit']
            or update.get('available', {}).get('version') != manifest['version']
            or update.get('available', {}).get('archive', {}).get('sha256') != manifest['artifacts'][ARCHIVE]
            or update.get('available', {}).get('archive', {}).get('name') != ARCHIVE
            or update.get('available', {}).get('archive', {}).get('url')
            != f"https://github.com/{REPO}/releases/download/v{manifest['version']}/{ARCHIVE}"):
        raise ValueError('default installed channel does not offer the exact hosted candidate')


def validate_installed_payload(store, installation, manifest, build_info):
    current = installation['current']
    if (not re.fullmatch(r'[0-9a-f]{64}', current.get('id', ''))
            or any(current.get(key) != manifest[key] for key in ('source_commit', 'version', 'target'))
            or any(build_info.get(key) != manifest[key]
                   for key in ('version', 'state_schema', 'installation_lock', 'launcher_redirect'))
            or build_info.get('os') != 'windows' or build_info.get('arch') != 'x86_64'):
        raise ValueError('installed selection or daemon metadata differs from the hosted candidate')
    selected = store / 'versions' / current['id'] / 'AgentDocker'
    if any(digest(selected / name) != expected for name, expected in manifest['binary_sha256'].items()):
        raise ValueError('installed executable bytes differ from the hosted candidate')


def validate_schema_rollback_refusal(command, before, after):
    current, previous = before.get('current', {}), before.get('previous') or {}
    if (type(current.get('state_schema')) is not int
            or type(previous.get('state_schema')) is not int
            or not 0 < previous['state_schema'] < current['state_schema']
            or command.get('exit_code') != 1 or command.get('stdout') != ''
            or command.get('stderr', '').strip()
            != 'Error: state schema differs; binary rollback cannot roll back the database'
            or before != after):
        raise ValueError('schema rollback must refuse exactly and preserve the entire installation')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--baseline-tag')
    parser.add_argument('--baseline-source')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('native Windows is required')
    identity(args.tag, args.source)
    if bool(args.baseline_tag) != bool(args.baseline_source):
        parser.error('both baseline tag and exact source are required')
    if args.baseline_tag:
        identity(args.baseline_tag, args.baseline_source)
        if args.baseline_tag == args.tag or args.baseline_source == args.source:
            parser.error('upgrade acceptance requires two different published releases')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    report = {'result': 'failed', 'tag': args.tag, 'source_commit': args.source,
              'baseline_tag': args.baseline_tag, 'baseline_source': args.baseline_source,
              'driver_sha256': digest(Path(__file__)), 'steps': [], 'commands': [],
              'rollback_disposition': 'not_requested',
              'scope': 'Hosted native Windows private-prefix install and default feed. With an actual baseline, equal schemas exercise update/rollback/reapply; a schema upgrade exercises update and exact rollback refusal without changing the installation. Neither restores an older database. Without a baseline no two-release lifecycle claim. Private installed daemon/connector service trials; no actual provider account, public tunnel, physical console, Start menu or logon/reboot claim.'}
    scratch = None

    def save():
        (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')

    def step(name, condition):
        report['steps'].append({'step': name, 'passed': bool(condition)})
        save()
        if not condition:
            raise AssertionError(name)

    try:
        before = stable()
        report['stable_before'] = before
        app, manifest = download(args.tag, args.source, output / 'candidate')
        baseline_app, baseline = (download(args.baseline_tag, args.baseline_source, output / 'baseline')
                                  if args.baseline_tag else (app, manifest))
        report['binary_sha256'] = manifest['binary_sha256']
        step('hosted archives, immutable tags and native installation contracts agree', True)
        scratch = Path(tempfile.mkdtemp(prefix='AgentDocker hosted ü '))
        report['scratch'] = str(scratch)
        prefix = scratch / 'local app data'
        store = prefix / 'AgentDocker/desktop'
        launcher = store / 'bin/agentdocker.exe'
        home = scratch / 'runtime'
        env = {k: v for k, v in os.environ.items()
               if not k.startswith('AGENTDOCKER_') and k not in ('GH_TOKEN', 'GITHUB_TOKEN')}
        env.update(AGENTDOCKER_HOME=str(home), AGENTDOCKER_NO_AUTOSTART='1',
                   AGENTDOCKER_SOCKET=rf'\\.\pipe\agentdocker-hosted-{uuid.uuid4().hex}')

        def command(*argv, executable=None, record_only=False):
            selected = executable or launcher
            result = subprocess.run([str(selected), *map(str, argv)], cwd=scratch, env=env,
                                    capture_output=True, text=True, encoding='utf-8', timeout=180)
            report['commands'].append({'argv': [str(selected), *map(str, argv)],
                                       'exit_code': result.returncode, 'stdout': result.stdout[-32768:],
                                       'stderr': result.stderr[-32768:]})
            save()
            if record_only:
                return report['commands'][-1]
            if result.returncode:
                raise AssertionError(f'hosted command failed: {argv}: {result.stderr[-2048:]}')
            return json.loads(result.stdout)

        def desktop(*argv, **kwargs):
            return command('desktop', '--prefix', prefix, *argv, **kwargs)

        def installed_matches(expected):
            # Offline daemon metadata has version/schema, not source identity.
            # Bind the selected installation to the verified executable bytes.
            validate_installed_payload(store, desktop('status')['installation'], expected,
                                       command('--build-info', executable=store / 'bin/agentd.exe'))
            return True

        initial = baseline_app / 'agentdocker.exe'
        cold = desktop('status', executable=initial)
        step('cold hosted status does not create the prefix or provider state',
             cold['installation'] is None and not prefix.exists() and not home.exists())
        preview = desktop('install', '--from', baseline_app, '--local-preview', '--preview', executable=initial)
        first = preview['candidate']['id']
        desktop('install', '--from', baseline_app, '--local-preview', '--expect-release', first,
                '--expect-current', 'none', executable=initial)
        installed = desktop('status')['installation']
        step('fresh hosted installation boots its exact immutable payload',
             installed['current']['id'] == first and installed['previous'] is None
             and installed_matches(baseline))
        checked = desktop('update', '--check')['update']
        validate_update(checked, manifest, bool(args.baseline_tag))
        step('installed default channel verifies the exact hosted preview without downloading',
             not (store / 'downloads').exists())
        if args.baseline_tag:
            planned = desktop('update')
            second = planned['candidate']['id']
            step('hosted update stages without activation', planned['preview']
                 and second != first and desktop('status')['installation']['current']['id'] == first)
            desktop('update', '--apply')
            active = desktop('status')['installation']
            step('hosted update preserves the actual prior release',
                 active['current']['id'] == second and active['previous']['id'] == first
                 and installed_matches(manifest))
            if baseline['state_schema'] != manifest['state_schema']:
                refused = desktop('rollback', '--local-preview', '--expect-current', second,
                                  '--expect-release', first, record_only=True)
                after = desktop('status')['installation']
                validate_schema_rollback_refusal(refused, active, after)
                report['rollback_disposition'] = 'refused_schema_change'
                report['rollback_refusal'] = {'command': refused, 'before': active, 'after': after}
                step('schema rollback refuses without changing the installed selection or bytes',
                     installed_matches(manifest) and not home.exists())
                unchanged = desktop('update', '--apply')
                step('newer-schema candidate remains active after refused rollback',
                     unchanged['update']['update_available'] is False
                     and desktop('status')['installation'] == active and installed_matches(manifest))
            else:
                desktop('rollback', '--local-preview', '--expect-current', second, '--expect-release', first)
                step('rollback restores the exact earlier hosted binaries',
                     installed_matches(baseline)
                     and desktop('status')['installation']['current']['id'] == first)
                desktop('update', '--apply')
                step('default feed reapplies the hosted candidate after rollback',
                     installed_matches(manifest)
                     and desktop('status')['installation']['current']['id'] == second)
                report['rollback_disposition'] = 'restored'
        else:
            unchanged = desktop('update', '--apply')
            step('current hosted release stays unchanged when no update exists',
                 unchanged['update']['update_available'] is False
                 and desktop('status')['installation']['current']['id'] == first)
        current = desktop('status')['installation']['current']
        selected = store / 'versions' / current['id'] / 'AgentDocker'
        step('active payload bytes match the hosted candidate and launchers retain their baseline',
             all(digest(selected / n) == h for n, h in manifest['binary_sha256'].items())
             and all(digest(store / 'bin' / n) == h for n, h in baseline['binary_sha256'].items()))
        report['bootstrap_binary_sha256'] = baseline['binary_sha256']
        for script, label in [('windows_service_smoke.py', 'daemon-service'),
                              ('windows_connector_service_smoke.py', 'connector-service')]:
            subprocess.run([sys.executable, str(Path(__file__).with_name(script)),
                            '--binary-dir', str(store / 'bin'), '--installed-prefix', str(prefix),
                            '--output', str(output / label)], env=env, check=True, timeout=600)
            service = json.loads((output / label / 'result.json').read_text(encoding='utf-8'))
            step('hosted installed ' + label + ' passes native lifecycle and owned cleanup',
                 service['result'] == 'passed' and service['installed_launchers']
                 and service['cleanup_errors'] == [] and not service.get('forced_processes')
                 and service['scratch_removed'] and service['steps']
                 and all(s['ok'] for s in service['steps'])
                 and all(service['binary_sha256'][n] == baseline['binary_sha256'][n]
                         for n in ['agentdocker.exe', 'agentd.exe']))
        step('service trials preserve the selected hosted payload',
             desktop('status')['installation']['current']['id'] == current['id']
             and all(digest(selected / n) == h for n, h in manifest['binary_sha256'].items()))
        uninstall = desktop('uninstall', '--preview')
        desktop('uninstall', '--expect-plan', uninstall['plan_id'])
        step('hosted bootstrap uninstalls without touching provider state',
             not (store / 'bin').exists() and not home.exists())
        report['stable_after'] = stable()
        step('stable release and Homebrew tap are unchanged', before == report['stable_after'])
        report['two_release_lifecycle'] = bool(args.baseline_tag)
        shutil.rmtree(scratch)
        step('owned hosted fixture scratch is removed', not scratch.exists())
        report['scratch_removed'] = True
        report['result'] = 'passed'
    except Exception as error:
        report['error'] = f'{type(error).__name__}: {error}'
        raise
    finally:
        # Failed scratch is evidence, never silently deleted or reported clean.
        save()


if __name__ == '__main__':
    main()

#!/usr/bin/env python3
"""Exercise native installation using the extracted Windows package only."""
import argparse
import hashlib
import json
import msvcrt
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--service', action='store_true')
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('this trial requires native Windows')
    app = args.binary_dir.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    scratch = Path(tempfile.mkdtemp(prefix='AgentDocker install ü '))
    prefix = scratch / 'local app data'
    store = prefix / 'AgentDocker/desktop'
    home = scratch / 'runtime'
    cli = app / 'agentdocker.exe'
    launcher = store / 'bin/agentdocker.exe'
    token = uuid.uuid4().hex
    env = {k: v for k, v in os.environ.items() if not k.startswith('AGENTDOCKER_')}
    env.update(AGENTDOCKER_HOME=str(home), AGENTDOCKER_NO_AUTOSTART='1',
               AGENTDOCKER_SOCKET=rf'\\.\pipe\agentdocker-install-{token}')
    names = ('agentdocker.exe', 'agentd.exe', 'agentdocker-ui.exe')
    report = {'result': 'failed', 'source_commit': json.loads((app / 'build.json').read_text())['source_commit'],
              'binary_sha256': {n: hashlib.sha256((app / n).read_bytes()).hexdigest() for n in names},
              'scope': 'Native private-prefix install and rollback. Second payload changes a fixture README only; a third metadata-only fixture exercises the local preview feed. No hosted update, actual provider, Start menu or reboot claim.',
              'scratch': str(scratch), 'service_requested': args.service, 'steps': [], 'commands': []}

    def save():
        (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')

    def run(*argv, executable=cli, good=True, raw=False):
        started = time.monotonic()
        result = subprocess.run([str(executable), *map(str, argv)], cwd=scratch, env=env,
                                capture_output=True, text=True, encoding='utf-8', timeout=45)
        report['commands'].append({'argv': [str(executable), *map(str, argv)],
                                   'exit_code': result.returncode, 'seconds': time.monotonic() - started,
                                   'stdout': result.stdout[-32768:], 'stderr': result.stderr[-32768:]})
        save()
        if (result.returncode == 0) != good:
            raise AssertionError(f'Unexpected exit {result.returncode}: {argv}: {result.stderr}')
        return result.stdout if raw or not good else json.loads(result.stdout)

    def desktop(*argv, **kwargs):
        return run('desktop', '--prefix', prefix, *argv, **kwargs)

    def step(name, condition):
        report['steps'].append({'name': name, 'passed': bool(condition)})
        save()
        if not condition:
            raise AssertionError(name)

    try:
        cold = desktop('status')
        step('cold status leaves prefix and runtime absent', cold['installation'] is None and not prefix.exists() and not home.exists())
        preview = desktop('install', '--from', app, '--local-preview', '--preview')
        first = preview['candidate']['id']
        step('preview reports the immutable candidate without writing', len(first) == 64 and not prefix.exists())
        desktop('install', '--from', app, good=False)
        desktop('install', '--from', app, '--local-preview', '--expect-release', '0' * 64, good=False)
        step('missing preview consent and changed release preserve the absent installation', not prefix.exists())
        desktop('install', '--from', app, '--local-preview', '--expect-release', first, '--expect-current', 'none')
        status = desktop('status', executable=launcher)
        step('installed bootstrap resolves the active payload', status['installation']['current']['id'] == first and status['installation']['previous'] is None)
        original = run('--version', raw=True)
        step('copied CLI forwards arguments and stdout', run('--version', executable=launcher, raw=True) == original)
        daemon = run('--build-info', executable=store / 'bin/agentd.exe')
        step('copied daemon forwards its metadata request without starting state', daemon['launcher_redirect'] == 2 and not home.exists())
        receipt = (store / 'launcher.json').read_bytes()
        second_app = scratch / 'second payload'
        shutil.copytree(app, second_app)
        with (second_app / 'README.txt').open('a', encoding='utf-8') as stream:
            stream.write('\nIsolated installation/rollback fixture; identical executable bytes.\n')
        second = desktop('install', '--from', second_app, '--local-preview', '--preview')['candidate']['id']
        step('fixture payload has a distinct content identity', second != first)
        desktop('install', '--from', second_app, '--local-preview', '--expect-current', first, '--expect-release', second)
        status = desktop('status', executable=launcher)['installation']
        step('activation retains exactly the previous release', status['current']['id'] == second and status['previous']['id'] == first and (store / 'launcher.json').read_bytes() == receipt)
        desktop('install', '--from', second_app, '--local-preview', '--expect-current', second)
        step('idempotent reinstallation preserves rollback', desktop('status')['installation']['previous']['id'] == first)
        before = (store / 'activation.json').read_bytes()
        desktop('rollback', '--local-preview', '--expect-current', first, good=False)
        review = desktop('rollback', '--local-preview', '--preview')
        step('rollback preview and stale guard preserve activation', review['candidate']['id'] == first and (store / 'activation.json').read_bytes() == before)
        desktop('rollback', '--local-preview', '--expect-current', second, '--expect-release', first)
        status = desktop('status', executable=launcher)['installation']
        step('rollback exchanges retained versions through the same bootstrap', status['current']['id'] == first and status['previous']['id'] == second and run('--version', executable=launcher, raw=True) == original)
        retained_readme = store / 'versions' / second / 'AgentDocker/README.txt'
        good_readme = retained_readme.read_bytes()
        retained_readme.write_bytes(good_readme + b'changed')
        desktop('rollback', '--local-preview', good=False)
        step('changed retained content cannot become active', desktop('status')['installation']['current']['id'] == first)
        retained_readme.write_bytes(good_readme)
        good_launcher = launcher.read_bytes()
        launcher.write_bytes(good_launcher + b'changed')
        desktop('status', good=False)
        run('--version', executable=launcher, raw=True, good=False)
        launcher.write_bytes(good_launcher)
        step('modified bootstrap is refused and restored exact bytes recover', run('--version', executable=launcher, raw=True) == original)
        update_app = scratch / 'update fixture'
        shutil.copytree(app, update_app)
        build = json.loads((update_app / 'build.json').read_text(encoding='utf-8'))
        build['version'] = '99.0.0-beta.1'
        (update_app / 'build.json').write_text(json.dumps(build), encoding='utf-8')
        archive = scratch / 'agentdocker-desktop-x86_64-pc-windows-msvc.zip'
        with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED) as bundle:
            for path in sorted(update_app.rglob('*')):
                if path.is_file():
                    bundle.write(path, 'AgentDocker/' + path.relative_to(update_app).as_posix())
        feed = {'format': 1, 'product': 'agentdocker', 'channel': 'preview', 'releases': [{
            'target': build['target'], 'version': build['version'], 'source_commit': build['source_commit'],
            'state_schema': build['state_schema'], 'signing': 'unsigned-preview', 'notarized': False,
            'archive': {'name': archive.name, 'sha256': hashlib.sha256(archive.read_bytes()).hexdigest(),
                        'bytes': archive.stat().st_size, 'url': 'file://' + str(archive)}}]}
        feed_path = scratch / 'updates-preview.json'
        feed_path.write_text(json.dumps(feed), encoding='utf-8')
        update_args = ('update', '--feed', 'file://' + str(feed_path), '--local-preview')
        checked = desktop(*update_args, '--check')
        step('Windows preview feed check reports the newer fixture without downloading',
             checked['update']['update_available'] and not (store / 'downloads').exists())
        update_preview = desktop(*update_args)
        third = update_preview['candidate']['id']
        step('Windows archive preview verifies and stages without activation',
             update_preview['preview'] and desktop('status')['installation']['current']['id'] == first)
        step('Windows update preview removes its temporary extracted payload',
             not list((store / 'downloads').glob('*/payload-*')))
        pin_path = store / 'pins' / (second + '.lock')
        with pin_path.open('r+b') as held:
            msvcrt.locking(held.fileno(), msvcrt.LK_NBLCK, 1)
            try:
                applied = desktop(*update_args, '--apply')
                step('activation preserves an inactive version whose lock is held',
                     applied['retention']['completed'] and (store / 'versions' / second).is_dir())
            finally:
                held.seek(0)
                msvcrt.locking(held.fileno(), msvcrt.LK_UNLCK, 1)
        step('Windows update apply removes its temporary extracted payload',
             not list((store / 'downloads').glob('*/payload-*')))
        desktop('install', '--from', update_app, '--local-preview', '--expect-current', third)
        step('idempotent active installation preserves retained inactive versions',
             (store / 'versions' / second).is_dir()
             and desktop('status')['installation']['previous']['id'] == first)
        status = desktop('status', executable=launcher)['installation']
        step('Windows preview update activates through the existing bootstrap',
             status['current']['id'] == third and status['previous']['id'] == first
             and (store / 'launcher.json').read_bytes() == receipt)
        retained = desktop('prune', '--preview', '--keep', '1')
        step('explicit additional retention keeps the old version', not retained['maintenance']['remove'])
        cleanup = desktop('prune', '--preview')
        step('unlocked inactive version becomes eligible for pruning',
             len(cleanup['maintenance']['remove']) == 1 and Path(cleanup['maintenance']['remove'][0]).name == second)
        unknown = store / 'versions/user-notes'
        unknown.mkdir()
        (unknown / 'keep.txt').write_text('driver-owned content must be preserved', encoding='utf-8')
        desktop('prune', '--expect-plan', cleanup['plan_id'], good=False)
        step('changed maintenance inventory invalidates the reviewed plan', (store / 'versions' / second).is_dir())
        cleanup = desktop('prune', '--preview')
        desktop('prune', '--expect-plan', cleanup['plan_id'])
        step('pruning removes only the unlocked verified version',
             not (store / 'versions' / second).exists() and (store / 'versions' / first).is_dir()
             and (store / 'versions' / third).is_dir() and (unknown / 'keep.txt').is_file())
        desktop('rollback', '--local-preview', '--expect-current', third)
        step('an update can roll back to the original immutable payload',
             desktop('status')['installation']['current']['id'] == first)
        feed['releases'][0]['archive']['sha256'] = '0' * 64
        feed_path.write_text(json.dumps(feed), encoding='utf-8')
        desktop(*update_args, '--apply', good=False)
        step('checksum mismatch preserves activation', desktop('status')['installation']['current']['id'] == first)
        feed['channel'] = 'stable'
        feed['releases'][0]['version'] = '99.0.0'
        feed_path.write_text(json.dumps(feed), encoding='utf-8')
        desktop(*update_args, '--check', good=False)
        step('unsigned Windows packages cannot enter a stable feed', desktop('status')['installation']['current']['id'] == first)
        if args.service:
            subprocess.run([sys.executable, str(Path(__file__).with_name('windows_service_smoke.py')),
                            '--binary-dir', str(store / 'bin'), '--installed-prefix', str(prefix),
                            '--output', str(output / 'service')], check=True, timeout=600)
            service = json.loads((output / 'service/result.json').read_text(encoding='utf-8'))
            step('installed stable launchers pass the isolated Task Scheduler lifecycle',
                 service['result'] == 'passed' and not service['cleanup_errors'])
            report['service'] = {'result': 'passed', 'steps': len(service['steps'])}
        foreign = store / 'bin/user-notes.txt'
        foreign.write_text('driver-owned launcher neighbor', encoding='utf-8')
        desktop('uninstall', '--preview', good=False)
        step('foreign launcher neighbor prevents uninstall without deleting anything',
             foreign.is_file() and launcher.is_file())
        foreign.unlink()
        uninstall = desktop('uninstall', '--preview', executable=launcher)
        desktop('uninstall', '--expect-plan', '0' * 64, executable=launcher, good=False)
        step('stale uninstall plan preserves launchers and activation',
             launcher.is_file() and desktop('status')['installation']['current']['id'] == first)
        desktop('uninstall', '--expect-plan', uninstall['plan_id'], executable=launcher)
        step('installed command retires its loaded bootstrap and deactivates atomically',
             not (store / 'bin').exists() and not (store / 'launcher.json').exists()
             and desktop('status')['installation'] is None)
        step('uninstall preserves immutable releases and unrecognized retained content',
             (store / 'versions' / first).is_dir() and (unknown / 'keep.txt').is_file())
        retirement = store / 'retired-launchers'
        foreign_retirement = retirement / str(uuid.uuid4())
        foreign_retirement.mkdir()
        (foreign_retirement / 'keep.txt').write_text('preserve unrecognized retirement')
        desktop('install', '--from', app, '--local-preview', '--expect-current', 'none')
        step('reinstall after uninstall restores the stable launcher',
             desktop('status', executable=launcher)['installation']['current']['id'] == first)
        step('reinstall collects closed retired launchers while preserving an unrecognized directory',
             set(retirement.iterdir()) == {foreign_retirement}
             and (foreign_retirement / 'keep.txt').read_text() == 'preserve unrecognized retirement')
        prune = desktop('prune', '--preview')
        desktop('prune', '--expect-plan', prune['plan_id'])
        step('unrecognized retirement does not prevent verified maintenance',
             (foreign_retirement / 'keep.txt').is_file())
        desktop('uninstall', executable=launcher, good=False)
        step('uninstall preserves unaccountable retirement and identifies the directory before deactivation',
             str(foreign_retirement.resolve()) in report['commands'][-1]['stderr']
             and desktop('status')['installation']['current']['id'] == first and launcher.is_file())
        (foreign_retirement / 'keep.txt').unlink()
        foreign_retirement.rmdir()
        oversized = [retirement / str(uuid.uuid4()) for _ in range(9)]
        for directory in oversized:
            directory.mkdir()
        desktop('uninstall', executable=launcher, good=False)
        step('retirement inventory bound refuses before deactivation and preserves unknown contents',
             'retired launcher inventory exceeds its bound' in report['commands'][-1]['stderr']
             and desktop('status')['installation']['current']['id'] == first
             and launcher.is_file() and all(directory.is_dir() for directory in oversized))
        for directory in oversized:
            directory.rmdir()
        desktop('uninstall')
        step('installation leaves provider state and daemon startup untouched', not home.exists())
        report['result'] = 'passed'
    except Exception as error:
        report['error'] = f'{type(error).__name__}: {error}'
        raise
    finally:
        # Keep failed scratch for diagnosis; never erase unexpected contents
        # as part of a failed trial's cleanup.
        if report['result'] == 'passed':
            shutil.rmtree(scratch)
            report['scratch_removed'] = not scratch.exists()
        save()


if __name__ == '__main__':
    main()

"""Hosted acceptance must reject wrong provenance/channels before execution."""
import copy
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
try:
    SPEC = importlib.util.spec_from_file_location('windows_hosted_smoke', ROOT / 'scripts/windows_hosted_smoke.py')
    HOSTED = importlib.util.module_from_spec(SPEC)
    SPEC.loader.exec_module(HOSTED)
finally:
    sys.path.pop(0)


class HostedWindowsAcceptance(unittest.TestCase):
    def setUp(self):
        self.tag, self.source = 'v0.2.0-beta.5', 'a' * 40
        self.manifest = {'source_commit': self.source, 'source_dirty': False,
                         'version': self.tag[1:], 'target': HOSTED.TARGET,
                         'installation_lock': 1, 'launcher_redirect': 2,
                         'artifacts': {HOSTED.ARCHIVE: 'b' * 64}}
        self.update = {'feed': HOSTED.PREVIEW_FEED, 'target': HOSTED.TARGET,
                       'channel': 'preview', 'update_available': False,
                       'preview_consent_required': False,
                       'available': {'source_commit': self.source, 'version': self.tag[1:],
                                     'archive': {'name': HOSTED.ARCHIVE, 'sha256': 'b' * 64,
                                                 'url': f'https://github.com/{HOSTED.REPO}/releases/download/{self.tag}/{HOSTED.ARCHIVE}'}}}

    def test_portable_only_wrong_source_or_dirty_payload_is_not_an_installed_baseline(self):
        HOSTED.validate_manifest(self.manifest, self.tag, self.source)
        for key, value in [('launcher_redirect', 1), ('installation_lock', 0),
                           ('source_commit', 'c' * 40), ('source_dirty', True),
                           ('version', '0.2.0-beta.4'), ('target', 'x86_64-unknown-linux-musl')]:
            with self.subTest(key=key):
                changed = dict(self.manifest, **{key: value})
                with self.assertRaises(ValueError):
                    HOSTED.validate_manifest(changed, self.tag, self.source)

    def test_tag_mismatch_refuses_before_downloading_or_extracting_executables(self):
        with tempfile.TemporaryDirectory() as root:
            with patch.object(HOSTED, 'api', side_effect=[
                    {'draft': False, 'prerelease': True, 'tag_name': self.tag},
                    {'object': {'type': 'commit', 'sha': 'f' * 40}}]), \
                    patch.object(HOSTED, 'gh') as download, patch.object(HOSTED, 'extract_checked') as extract:
                with self.assertRaisesRegex(ValueError, 'immutable tag'):
                    HOSTED.download(self.tag, self.source, Path(root) / 'trial')
                download.assert_not_called()
                extract.assert_not_called()

    def test_default_channel_requires_the_exact_candidate_and_lifecycle_expectation(self):
        HOSTED.validate_update(self.update, self.manifest, False)
        for path, value in [(('feed',), 'file:///local.json'), (('channel',), 'stable'),
                            (('target',), 'other'), (('update_available',), True),
                            (('preview_consent_required',), True),
                            (('available', 'source_commit'), 'c' * 40),
                            (('available', 'archive', 'sha256'), 'd' * 64),
                            (('available', 'archive', 'url'), 'https://example.invalid/other.zip')]:
            with self.subTest(path=path):
                changed = copy.deepcopy(self.update)
                target = changed
                for field in path[:-1]:
                    target = target[field]
                target[path[-1]] = value
                with self.assertRaises(ValueError):
                    HOSTED.validate_update(changed, self.manifest, False)
        with self.assertRaises(ValueError):
            HOSTED.validate_update(self.update, self.manifest, True)

    def test_only_exact_prerelease_and_source_can_reach_the_hosted_api(self):
        for tag, source in [('main', self.source), ('v0.2.0', self.source),
                            ('../../v0.2.0-beta.5', self.source), (self.tag, 'main')]:
            with self.subTest(tag=tag, source=source):
                with self.assertRaises(ValueError):
                    HOSTED.identity(tag, source)


if __name__ == '__main__':
    unittest.main()

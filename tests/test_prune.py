import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[1]
FAKE_AWS = '''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
fixture = json.loads(os.environ['AWS_FIXTURE'])
if args[:2] == ['s3', 'cp']:
    bucket, key = args[2].removeprefix('s3://').split('/', 1)
    manifest = fixture['manifests'].get(bucket)
    if manifest is None:
        print('NoSuchKey: 404', file=sys.stderr); sys.exit(1)
    print(json.dumps(manifest))
elif args[:2] == ['s3api', 'list-objects-v2']:
    bucket = args[args.index('--bucket') + 1]
    for key, modified in fixture['objects'].get(bucket, []):
        print(key, modified, sep='\\t')
elif args[:2] == ['s3', 'rm']:
    with open(os.environ['AWS_DELETIONS'], 'a') as f: f.write(args[2] + '\\n')
else:
    print('Unexpected aws call: ' + repr(args), file=sys.stderr); sys.exit(2)
'''


class PruneTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        aws = self.dir / 'aws'
        aws.write_text(FAKE_AWS)
        aws.chmod(0o755)
        self.deletions = self.dir / 'deleted'
        self.fixture = {
            'manifests': {'r2': {'version': 'v2'}, 'secondary': {'version': 'v1'}},
            'objects': {
                'r2': [(key, '2026-01-01T00:00:00Z') for key in [
                    'codex/v2/windows/tool.zip', 'codex/v0/windows/tool.zip', 'codex/latest.json', 'codex/install.sh']],
                'secondary': [(key, '2026-01-01T00:00:00Z') for key in [
                    'codex/v2/windows/tool.zip', 'codex/v1/windows/tool.zip', 'codex/v0/windows/tool.zip',
                    'codex/latest.json', 'codex/install.sh', 'codex/install.ps1', 'codex/v0/']],
            },
        }
        self.env = {**os.environ, 'PATH': str(self.dir) + os.pathsep + os.environ['PATH'],
                    'R2_S3_ENDPOINT': 'https://r2.example', 'R2_BUCKET': 'r2',
                    'AWS_ACCESS_KEY_ID': 'test', 'AWS_SECRET_ACCESS_KEY': 'test',
                    'SECONDARY_S3_ENDPOINT': 'https://secondary.example', 'SECONDARY_S3_BUCKET': 'secondary',
                    'SECONDARY_S3_ACCESS_KEY_ID': 'test', 'SECONDARY_S3_SECRET_ACCESS_KEY': 'test',
                    'PROVIDERS': 'codex', 'AWS_DELETIONS': str(self.deletions)}

    def run_prune(self, **env):
        return subprocess.run(['bash', str(ROOT / 'scripts/prune.sh')],
                              env={**self.env, 'AWS_FIXTURE': json.dumps(self.fixture), **env},
                              text=True, capture_output=True)

    def deleted(self):
        return set(self.deletions.read_text().splitlines()) if self.deletions.exists() else set()

    def test_preserves_both_live_versions_and_installers(self):
        result = self.run_prune()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.deleted(), {'s3://r2/codex/v0/windows/tool.zip', 's3://secondary/codex/v0/windows/tool.zip'})

    def test_dry_run_does_not_delete(self):
        result = self.run_prune(PRUNE_DRY_RUN='true')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('would delete secondary', result.stdout)
        self.assertEqual(self.deleted(), set())

    def test_invalid_secondary_manifest_stops_secondary_deletion(self):
        self.fixture['manifests']['secondary'] = {'version': '../other'}
        result = self.run_prune()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(x.startswith('s3://secondary/') for x in self.deleted()))

    def test_unpublished_secondary_can_be_initialized(self):
        del self.fixture['manifests']['secondary']
        result = self.run_prune()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn('s3://secondary/codex/v2/windows/tool.zip', self.deleted())

    def workflow_function(self, name):
        lines = (ROOT / '.github/workflows/mirror.yml').read_text().splitlines()
        start = lines.index('          ' + name + '() {')
        end = lines.index('          }', start + 1)
        return textwrap.dedent('\n'.join(lines[start:end + 1]))

    def test_best_effort_still_prunes_secondary(self):
        source = self.workflow_function('prune_provider') + '\nprune_provider codex\n'
        result = subprocess.run(['bash', '-c', source], cwd=ROOT, text=True, capture_output=True,
                                env={**self.env, 'AWS_FIXTURE': json.dumps(self.fixture), 'SECONDARY_S3_REQUIRED': 'false'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('s3://secondary/codex/v0/windows/tool.zip', self.deleted())

    def test_secondary_sync_is_required_by_default(self):
        source = self.workflow_function('secondary_s3_required') + '\nsecondary_s3_required\n'
        for value, expected in [('', 0), ('true', 0), ('false', 1)]:
            result = subprocess.run(['bash', '-c', source], env={**self.env, 'SECONDARY_S3_REQUIRED': value})
            self.assertEqual(result.returncode, expected)

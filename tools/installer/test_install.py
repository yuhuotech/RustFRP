"""Exercise the real POSIX installer with isolated fake network/agent executables."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / 'installer/install.sh'


class InstallerTests(unittest.TestCase):
    def run_install(self, *, digest=None, extra=(), arch='x86_64', fail=False):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            tools = root / 'tools'
            tools.mkdir()
            payload = b'#!/bin/sh\nprintf "%s\\n" "$@" > "$HOME/args"\n'
            (root / 'payload').write_bytes(payload)
            (root / 'digest').write_text(digest or hashlib.sha256(payload).hexdigest())
            for name, body in {
                'uname': f'#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo {arch};; esac\n',
                'curl': '''#!/bin/sh
[ "$FAIL" = 1 ] && exit 22
while [ "$#" -gt 0 ]; do
 case "$1" in https://*) url=$1;; -o) shift; out=$1;; esac
 shift
done
case "$url" in *.sha256) cp "$HOME/digest" "$out";; *) cp "$HOME/payload" "$out";; esac
''',
            }.items():
                path = tools / name
                path.write_text(body)
                path.chmod(0o700)
            target = root / '.rustfrp/bin/rustfrp-agent'
            target.parent.mkdir(parents=True)
            target.write_text('existing')
            env = dict(os.environ, HOME=str(root), PATH=f'{tools}:' + os.environ['PATH'], FAIL=str(int(fail)))
            name = "中文 ' \" $(touch nope); `id`"
            proc = subprocess.run(['sh', str(SCRIPT), '--controller', 'localhost:7000',
                                   '--enroll-token', 'fake', '--name', name,
                                   '--download-base', 'https://example.test/downloads', *extra],
                                  env=env, capture_output=True, text=True)
            args = (root / 'args').read_text().splitlines() if (root / 'args').exists() else None
            return proc, args, target.read_bytes(), list((root / '.rustfrp').glob('install.*')), name

    def test_pin_is_forwarded_without_shell_interpolation(self):
        pin = 'a' * 64
        proc, args, _, leftovers, _ = self.run_install(extra=('--tls-pin', pin))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(args[-2:], ['--tls-pin', pin])
        self.assertEqual(leftovers, [])

    def test_success_preserves_literals_and_cleans_up(self):
        proc, args, binary, leftovers, name = self.run_install()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(args[:8], ['--mode', 'client', '--controller', 'localhost:7000', '--enroll-token', 'fake', '--name', name])
        self.assertEqual(args[8], '--state-dir')
        self.assertTrue(binary.startswith(b'#!/bin/sh'))
        self.assertEqual(leftovers, [])

    def test_errors_preserve_existing_agent(self):
        for kwargs in ({'digest': '0' * 64}, {'digest': 'invalid'}, {'fail': True},
                       {'arch': 'mips'}, {'extra': ('--unknown', 'x')},
                       {'extra': ('--name',)}, {'extra': ('--download-base', 'http://example.test')}):
            with self.subTest(kwargs=kwargs):
                proc, args, binary, leftovers, _ = self.run_install(**kwargs)
                self.assertNotEqual(proc.returncode, 0)
                self.assertIsNone(args)
                self.assertEqual(binary, b'existing')
                self.assertEqual(leftovers, [])


if __name__ == '__main__':
    unittest.main()

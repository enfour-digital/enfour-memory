"""Owned settings upgrade atomically; conflicting client settings stay untouched."""
from pathlib import Path
import json
import shutil
import subprocess
import tempfile
import tomllib
import unittest

SOURCE=Path(__file__).parents[1]/'scripts/install-clients'

class ClientInstall(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory()
        self.root=Path(self.temp.name)
        self.project=self.root/'project'
        (self.project/'scripts').mkdir(parents=True)
        self.script=self.project/'scripts/install-clients'
        shutil.copy2(SOURCE,self.script)
        self.home=self.root/'home'
        self.targets=[self.root/'source.toml',self.home/'.grok/config.toml']
        self.entry={'command':str(self.project/'scripts/enfour'),'args':['connect'],'startup_timeout_sec':30,'tool_timeout_sec':60}
        for target in self.targets:
            target.parent.mkdir(parents=True,exist_ok=True)
            target.write_text('theme = "dark"\n[mcp_servers.existing]\ncommand = "preserve-me"\n[mcp_servers.enfour-memory]\n'+''.join(f'{k} = {json.dumps(v)}\n' for k,v in self.entry.items())+'enabled = false # preserve this user setting\n')
            target.chmod(0o600)
        self.before=[tomllib.loads(t.read_text()) for t in self.targets]

    def tearDown(self):self.temp.cleanup()

    def run_installer(self):
        return subprocess.run([str(self.script),'--home',str(self.home),'--codex-source',str(self.targets[0]),'--client','codex','--client','grok'],capture_output=True,text=True)

    def test_upgrade_preserves_settings_and_is_idempotent(self):
        result=self.run_installer()
        self.assertEqual(result.returncode,0,result.stderr)
        for target,expected in zip(self.targets,self.before):
            expected['mcp_servers']['enfour-memory']['tool_timeout_sec']=180
            self.assertEqual(tomllib.loads(target.read_text()),expected)
            self.assertIn('# preserve this user setting',target.read_text())
            self.assertEqual(target.stat().st_mode & 0o777,0o600)
        backups=list((self.project/'state/client-backups').iterdir())
        self.assertEqual(len(backups),1)
        self.assertTrue(all(p.stat().st_mode & 0o777==0o600 for p in backups[0].iterdir()))
        self.assertEqual(self.run_installer().returncode,0)
        self.assertEqual(list((self.project/'state/client-backups').iterdir()),backups)

    def test_conflict_prevents_all_writes(self):
        target=self.targets[1]
        target.write_text(target.read_text().replace('tool_timeout_sec = 60','tool_timeout_sec = 45'))
        before=[t.read_bytes() for t in self.targets]
        self.assertNotEqual(self.run_installer().returncode,0)
        self.assertEqual([t.read_bytes() for t in self.targets],before)
        self.assertFalse((self.project/'state').exists())

    def test_default_only_changes_codex(self):
        grok_before = self.targets[1].read_bytes()
        result = subprocess.run([str(self.script), '--home', str(self.home)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.targets[1].read_bytes(), grok_before)
        self.assertTrue((self.home / '.codex/config.toml').is_file())

    def test_source_replaces_managed_codex_path(self):
        link = self.home / '.codex/config.toml'
        link.parent.mkdir(parents=True)
        link.symlink_to(self.targets[0])
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(link.is_symlink())
        self.assertEqual(tomllib.loads(link.read_text())['mcp_servers']['enfour-memory']['tool_timeout_sec'], 180)

    def test_symlink_without_source_is_not_replaced(self):
        link = self.home / '.codex/config.toml'
        link.parent.mkdir(parents=True)
        link.symlink_to(self.targets[0])
        before = self.targets[0].read_bytes()
        result = subprocess.run([str(self.script), '--home', str(self.home)], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(link.is_symlink())
        self.assertEqual(self.targets[0].read_bytes(), before)

if __name__=='__main__':unittest.main()

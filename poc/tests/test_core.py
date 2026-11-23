import unittest, tempfile, os, json, time
from pathlib import Path
from unittest.mock import patch
from dsg.core import scan, plan, deny_command, apply_plan, config
class GuardTests(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name);self.state=self.root/'.state';os.environ['DSG_STATE_HOME']=str(self.state)
 def tearDown(self):
  os.environ.pop('DSG_STATE_HOME',None);os.environ.pop('DSG_POC_ALLOW_DELETE',None);self.tmp.cleanup()
 def test_scan_known_outputs_only(self):
  (self.root/'build').mkdir();(self.root/'build'/'foo').write_bytes(b'x'*100);(self.root/'private').mkdir();(self.root/'private'/'secret').write_text('do not touch')
  s=scan(self.root);self.assertEqual(s['total_bytes'],100);self.assertEqual(s['entries'][0]['path'],'build')
 def test_expired_plan_no_unmanaged_files(self):
  (self.root/'build').mkdir();(self.root/'build'/'x').write_text('x');t=time.time()-86400*9;os.utime(self.root/'build',(t,t))
  p=plan(self.root);self.assertEqual(len(p['candidates']),1)
  with self.assertRaisesRegex(ValueError,'DSG_POC_ALLOW_DELETE'):apply_plan(p['plan_id'],True)
  self.assertTrue((self.root/'build').exists())
 def test_plan_invalid_after_modification(self):
  (self.root/'build').mkdir();t=time.time()-86400*9;os.utime(self.root/'build',(t,t));p=plan(self.root)
  (self.root/'build'/'new').write_text('changed');os.environ['DSG_POC_ALLOW_DELETE']='YES'
  with self.assertRaisesRegex(ValueError,'modified'):apply_plan(p['plan_id'],True)
 def test_symlink_not_scanned(self):
  outside=self.root/'external';outside.mkdir();(outside/'important').write_text('keep');(self.root/'build').symlink_to(outside,target_is_directory=True)
  self.assertEqual(scan(self.root)['total_bytes'],0)
 def test_deny_dangerous(self):
  for cmd in ['docker volume prune','docker system prune -a --volumes','docker compose down -v','rm -rf /tmp/test']:
   self.assertEqual(deny_command(cmd)['decision'],'deny',cmd)
 def test_protected_name_even_if_configured(self):
  (self.root/'data').mkdir();(self.root/'data'/'db').write_text('must persist')
  (self.root/'.dsg.json').write_text('{"scan_dirs":["data"],"gc_min_age_hours":0}')
  p=plan(self.root);self.assertEqual(p['candidates'],[])
  self.assertTrue((self.root/'data'/'db').exists())
 def test_unknown_cleanup_cannot_run(self):
  with self.assertRaises(ValueError):apply_plan('0000000000000000',False)
 def test_config_validation(self):
  (self.root/'.dsg.json').write_text('{"warning_percent":95,"critical_percent":90}')
  with self.assertRaises(ValueError):config(self.root)

 def test_project_protection_cannot_remove_defaults(self):
  (self.root/'data').mkdir()
  for protected_names in ([], ['fixture-private']):
   with self.subTest(protected_names=protected_names):
    (self.root/'.dsg.json').write_text(json.dumps({
     'scan_dirs': ['data'], 'protected_names': protected_names, 'gc_min_age_hours': 0,
    }))
    self.assertEqual(plan(self.root)['candidates'], [])

 def test_plan_keeps_ancestors_of_default_and_project_protected_entries(self):
  fixtures = ('build/sub/deep/Data/fixture.txt', 'target/sub/fixture-private', 'dist/artifact.txt')
  for relative in fixtures:
   fixture = self.root/relative
   fixture.parent.mkdir(parents=True, exist_ok=True)
   fixture.write_text('synthetic')
  (self.root/'.dsg.json').write_text(json.dumps({
   'protected_names': ['fixture-private'], 'gc_min_age_hours': 0,
  }))
  self.assertEqual([c['relative_path'] for c in plan(self.root)['candidates']], ['dist'])

 def test_plan_rejects_a_tree_with_an_unresolved_entry(self):
  build = self.root/'build'
  build.mkdir()
  (build/'linked').symlink_to(self.root/'missing')
  (self.root/'.dsg.json').write_text('{"gc_min_age_hours":0}')
  self.assertEqual(plan(self.root)['candidates'], [])

 def test_plan_rejects_an_incomplete_scan(self):
  build = self.root/'build'
  build.mkdir()
  for index in range(3):
   (build/f'artifact-{index}.txt').write_text('synthetic')
  (self.root/'.dsg.json').write_text('{"gc_min_age_hours":0,"max_scan_files":2}')
  self.assertEqual(plan(self.root)['candidates'], [])

 def test_apply_rechecks_nested_protection_and_preserves_all_candidates(self):
  build = self.root/'build'
  dist = self.root/'dist'
  nested = build/'nested'
  nested.mkdir(parents=True)
  dist.mkdir()
  (dist/'artifact.txt').write_text('synthetic')
  old = time.time()-86400*9
  os.utime(build, (old, old))
  os.utime(dist, (old, old))
  proposed = plan(self.root)
  self.assertEqual([item['relative_path'] for item in proposed['candidates']], ['dist', 'build'])
  original = nested/'data'
  original.mkdir()
  (original/'fixture.txt').write_text('synthetic protected content')
  os.environ['DSG_POC_ALLOW_DELETE'] = 'YES'
  with patch('dsg.core.shutil.rmtree') as delete:
   with self.assertRaisesRegex(ValueError, r'(?i)protect|unsafe|unknown|incomplete'):
    apply_plan(proposed['plan_id'], True)
   delete.assert_not_called()
  self.assertTrue((dist/'artifact.txt').exists())
  self.assertTrue((original/'fixture.txt').exists())

 def test_apply_uses_the_current_project_policy(self):
  build = self.root/'build'
  build.mkdir()
  (build/'fixture-private').write_text('synthetic')
  old = time.time()-86400*9
  os.utime(build, (old, old))
  proposed = plan(self.root)
  self.assertEqual(len(proposed['candidates']), 1)
  (self.root/'.dsg.json').write_text('{"protected_names":["fixture-private"]}')
  os.environ['DSG_POC_ALLOW_DELETE'] = 'YES'
  with patch('dsg.core.shutil.rmtree') as delete:
   with self.assertRaisesRegex(ValueError, r'(?i)protect|unsafe|unknown|incomplete'):
    apply_plan(proposed['plan_id'], True)
   delete.assert_not_called()
  self.assertTrue((build/'fixture-private').exists())

if __name__=='__main__':unittest.main()

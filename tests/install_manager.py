#!/usr/bin/env python3
import hashlib,os,pathlib,shutil,subprocess,tarfile,tempfile,unittest
ROOT=pathlib.Path(__file__).resolve().parents[1]; SCRIPT=ROOT/"scripts/vibe-install.sh"; ASSET="vibe-sdk-x86_64-unknown-linux-gnu.tar.gz"
class T(unittest.TestCase):
 def setUp(self):
  self.t=pathlib.Path(tempfile.mkdtemp()); self.server=self.t/"server"; self.cmd=self.t/"cmd"; self.dest=self.t/"install"; self.server.mkdir(); self.cmd.mkdir()
  self.command("uname",'#!/bin/sh\n[ "$1" = "-s" ] && echo Linux || echo x86_64\n'); self.command("gh",'#!/bin/sh\nprintf "%s\\n" "$*" >> "$MOCK_GH_LOG"\nif [ "$1" = api ]; then echo "${MOCK_SHA:-0123456789abcdef0123456789abcdef01234567}"; exit 0; fi\n[ "${MOCK_ATTEST_FAIL:-0}" != 1 ]\n')
  self.command("curl",'''#!/bin/sh
set -eu
printf "%s\\n" "$*" >> "$MOCK_CURL_LOG"
out=; url=
while [ $# -gt 0 ]; do case $1 in -o) out=$2;shift 2;;-w|--retry|--proto)shift 2;;--tlsv1.2|-f|-L|-s|-S|-I|-fsSIL)shift;;*)url=$1;shift;;esac;done
if echo "$url"|grep -q '/releases/latest$';then printf '%s/test/repo/releases/tag/%s' "$VIBE_INSTALL_DOWNLOAD_URL" "${MOCK_LATEST:-v2.0.0}";exit 0;fi
[ "${MOCK_NETWORK_FAIL:-0}" != 1 ]||exit 22
name=${url##*/};rest=${url#*/download/};version=${rest%%/*};cp "$MOCK_SERVER/$version/$name" "$out"
''')
  self.env=os.environ|{"PATH":f"{self.cmd}:{os.environ['PATH']}","MOCK_SERVER":str(self.server),"MOCK_GH_LOG":str(self.t/"gh.log"),"MOCK_CURL_LOG":str(self.t/"curl.log"),"VIBE_INSTALL_REPOSITORY":"test/repo","VIBE_INSTALL_DOWNLOAD_URL":"https://mock.invalid","VIBE_INSTALL_DIR":str(self.dest)}; self.release("v1.0.0","one");self.release("v2.0.0","two")
 def tearDown(self):shutil.rmtree(self.t)
 def command(self,n,b):p=self.cmd/n;p.write_text(b);p.chmod(0o755)
 def release(self,v,text,bad=False):
  d=self.server/v;d.mkdir();q=self.t/f"p-{v}";q.mkdir();x=q/"vibe";x.write_text(f"#!/bin/sh\necho {text}\n");x.chmod(0o755)
  sums=[]
  for asset,entry in [(ASSET,"vibe"),(ASSET.replace("vibe-sdk-","vibe-runtime-"),"vibe-runtime")]:
   with tarfile.open(d/asset,"w:gz")as f:f.add(x,arcname=entry)
   h=hashlib.sha256((d/asset).read_bytes()).hexdigest();sums.append(f"{'0'*64 if bad else h}  {asset}\n")
  (d/"SHA256SUMS").write_text("".join(sums))
 def runit(self,*a,env=None):return subprocess.run([str(SCRIPT),*a],env=env or self.env,text=True,capture_output=True)
 def install1(self):r=self.runit("install","--version","v1.0.0","--non-interactive");self.assertEqual(r.returncode,0,r.stderr)
 def test_help(self):self.assertEqual(self.runit("--help").returncode,0)
 def test_fresh_repeat(self):self.install1();self.assertEqual(subprocess.check_output([self.dest/"vibe"],text=True).strip(),"one");before=os.readlink(self.dest/"vibe");self.assertEqual(self.runit("install","--version","v1.0.0").returncode,0);self.assertEqual(before,os.readlink(self.dest/"vibe"))
 def test_upgrade_latest(self):self.install1();r=self.runit("upgrade");self.assertEqual(r.returncode,0,r.stderr);self.assertEqual(subprocess.check_output([self.dest/"vibe"],text=True).strip(),"two")
 def test_remove_only_managed(self):self.dest.mkdir();keep=self.dest/"keep";keep.write_text("safe");self.install1();self.assertEqual(self.runit("remove").returncode,0);self.assertTrue(keep.exists());self.assertFalse((self.dest/"vibe").exists())
 def test_network_failure_preserves(self):self.install1();self.assertNotEqual(self.runit("upgrade","--version","v2.0.0",env=self.env|{"MOCK_NETWORK_FAIL":"1"}).returncode,0);self.assertEqual(subprocess.check_output([self.dest/"vibe"],text=True).strip(),"one")
 def test_checksum_failure_preserves(self):self.install1();self.release("v3.0.0","bad",True);self.assertNotEqual(self.runit("upgrade","--version","v3.0.0").returncode,0);self.assertEqual(subprocess.check_output([self.dest/"vibe"],text=True).strip(),"one")
 def test_attestation_failure_preserves(self):self.install1();self.assertNotEqual(self.runit("upgrade","--version","v2.0.0",env=self.env|{"MOCK_ATTEST_FAIL":"1"}).returncode,0);self.assertEqual(subprocess.check_output([self.dest/"vibe"],text=True).strip(),"one")
 def test_unsupported(self):self.command("uname",'#!/bin/sh\n[ "$1" = "-s" ]&&echo Plan9||echo mips\n');self.assertIn("unsupported platform",self.runit("install","--version","v1.0.0").stderr)
 def test_permission_invalid_destination(self):p=self.t/"file";p.write_text("x");self.assertNotEqual(self.runit("install","--version","v1.0.0","--install-dir",str(p)).returncode,0)
 def test_unmanaged_not_overwritten_or_removed(self):self.dest.mkdir();p=self.dest/"vibe";p.write_text("owned");self.assertNotEqual(self.runit("install","--version","v1.0.0").returncode,0);self.assertNotEqual(self.runit("remove").returncode,0);self.assertEqual(p.read_text(),"owned")
 def test_lock_blocks_parallel(self):self.dest.mkdir();(self.dest/".vibe-install.lock").mkdir();self.assertIn("another",self.runit("install","--version","v1.0.0").stderr)
 def test_attestation_identity_is_pinned(self):
  self.install1();log=(self.t/"gh.log").read_text().splitlines()
  self.assertIn("api repos/vibe-backend/sdk/commits/refs/tags/v1.0.0 --jq .sha",log)
  verify=[l for l in log if l.startswith("attestation verify ")];self.assertEqual(len(verify),1)
  self.assertTrue(verify[0].endswith(" --repo vibe-backend/sdk --signer-workflow vibe-backend/sdk/.github/workflows/release.yml --source-ref refs/tags/v1.0.0 --source-digest 0123456789abcdef0123456789abcdef01234567 --deny-self-hosted-runners"),verify[0])
 def test_bad_source_sha_fails(self):self.assertNotEqual(self.runit("install","--version","v1.0.0",env=self.env|{"MOCK_SHA":"not-a-sha"}).returncode,0);self.assertFalse((self.dest/"vibe").exists())
 def test_rc_install_attestation_identity_is_pinned(self):
  self.release("v1.2.3-rc.1","rc");r=self.runit("install","--version","v1.2.3-rc.1");self.assertEqual(r.returncode,0,r.stderr);self.assertEqual(subprocess.check_output([self.dest/"vibe"],text=True).strip(),"rc")
  log=(self.t/"gh.log").read_text().splitlines();self.assertIn("api repos/vibe-backend/sdk/commits/refs/tags/v1.2.3-rc.1 --jq .sha",log)
  verify=[l for l in log if l.startswith("attestation verify ")];self.assertEqual(len(verify),1)
  self.assertTrue(verify[0].endswith(" --repo vibe-backend/sdk --signer-workflow vibe-backend/sdk/.github/workflows/release.yml --source-ref refs/tags/v1.2.3-rc.1 --source-digest 0123456789abcdef0123456789abcdef01234567 --deny-self-hosted-runners"),verify[0])
 def test_upgrade_to_rc_then_stable(self):
  self.install1();self.release("v1.2.3-rc.1","rc")
  for version,text in [("v1.2.3-rc.1","rc"),("v2.0.0","two")]:
   with self.subTest(version=version):r=self.runit("upgrade","--version",version);self.assertEqual(r.returncode,0,r.stderr);self.assertEqual(subprocess.check_output([self.dest/"vibe"],text=True).strip(),text)
 def test_invalid_versions_rejected(self):
  for action in ["install","upgrade"]:
   for version in ["v1.2.3-rc","v1.2.3-rc.01","v1.2.3-beta.1","v1.2.3-rc.1+build","v01.2.3","1.2.3-rc.1","v1.2.3-rc.1-rc.2","v1.2.3-RC.1","x\nv1.2.3-rc.1\ny","v1.2.3-rc.1\n","v1.02.3","v1.2.03"]:
    with self.subTest(action=action,version=version):r=self.runit(action,"--version",version);self.assertNotEqual(r.returncode,0);self.assertIn("invalid release version",r.stderr);self.assertFalse((self.t/"gh.log").exists());self.assertFalse((self.t/"curl.log").exists())
 def test_rc_verification_failure_preserves(self):
  self.install1();before=os.readlink(self.dest/"vibe");self.release("v1.2.3-rc.1","bad",True);self.release("v1.2.3-rc.2","rc")
  for version,env,error in [("v1.2.3-rc.1",self.env,"checksum verification failed"),("v1.2.3-rc.2",self.env|{"MOCK_ATTEST_FAIL":"1"},"GitHub artifact attestation verification failed")]:
   with self.subTest(version=version):r=self.runit("upgrade","--version",version,env=env);self.assertNotEqual(r.returncode,0);self.assertIn(error,r.stderr);self.assertEqual(os.readlink(self.dest/"vibe"),before);self.assertEqual(subprocess.check_output([self.dest/"vibe"],text=True).strip(),"one")
 def test_runtime_flags_env_and_coexistence(self):
  self.install1();sdk=os.readlink(self.dest/"vibe")
  for flags,env in [(('--runtime',),self.env),((),self.env|{"VIBE_INSTALL_BINARY":"runtime"})]:
   r=self.runit("install","--version","v1.0.0",*flags,env=env);self.assertEqual(r.returncode,0,r.stderr)
   self.assertEqual(subprocess.check_output([self.dest/"vibe-runtime"],text=True).strip(),"one")
   self.assertEqual(os.readlink(self.dest/"vibe"),sdk)
  self.assertEqual(self.runit("remove","--runtime").returncode,0)
  self.assertFalse((self.dest/"vibe-runtime").exists());self.assertEqual(os.readlink(self.dest/"vibe"),sdk)
 def test_runtime_only_and_flag_precedence(self):
  r=self.runit("install","--version","v1.0.0","--runtime",env=self.env|{"VIBE_INSTALL_BINARY":"invalid"});self.assertEqual(r.returncode,0,r.stderr)
  self.assertFalse((self.dest/"vibe").exists())
 def test_invalid_binary_before_network(self):
  r=self.runit("install","--version","v1.0.0",env=self.env|{"VIBE_INSTALL_BINARY":"invalid"})
  self.assertIn("VIBE_INSTALL_BINARY must be sdk or runtime",r.stderr);self.assertNotEqual(r.returncode,0)
  self.assertFalse((self.t/"curl.log").exists())
 def test_sdk_flag_overrides_runtime_environment(self):
  r=self.runit("install","--version","v1.0.0","--sdk",env=self.env|{"VIBE_INSTALL_BINARY":"runtime"})
  self.assertEqual(r.returncode,0,r.stderr);self.assertTrue((self.dest/"vibe").exists());self.assertFalse((self.dest/"vibe-runtime").exists())
 def test_empty_binary_rejected(self):
  r=self.runit("install","--version","v1.0.0",env=self.env|{"VIBE_INSTALL_BINARY":""})
  self.assertNotEqual(r.returncode,0);self.assertIn("VIBE_INSTALL_BINARY must be sdk or runtime",r.stderr)
  self.assertFalse((self.t/"curl.log").exists())
 def test_runtime_rc_identity_and_failure_preserves(self):
  self.release("v1.0.0-rc.2","rc")
  r=self.runit("install","--version","v1.0.0-rc.2","--runtime");self.assertEqual(r.returncode,0,r.stderr)
  before=os.readlink(self.dest/"vibe-runtime")
  line=(self.t/"gh.log").read_text().splitlines()[1]
  archive=line.split()[2]
  self.assertEqual(line,f"attestation verify {archive} --repo vibe-backend/sdk --signer-workflow vibe-backend/sdk/.github/workflows/release.yml --source-ref refs/tags/v1.0.0-rc.2 --source-digest 0123456789abcdef0123456789abcdef01234567 --deny-self-hosted-runners")
  self.release("v3.0.0","bad",True)
  for version,env,error in [("v3.0.0",self.env,"checksum verification failed"),("v2.0.0",self.env|{"MOCK_ATTEST_FAIL":"1"},"attestation verification failed")]:
   r=self.runit("upgrade","--runtime","--version",version,env=env);self.assertNotEqual(r.returncode,0);self.assertIn(error,r.stderr)
   self.assertEqual(os.readlink(self.dest/"vibe-runtime"),before)
   self.assertEqual(subprocess.check_output([self.dest/"vibe-runtime"],text=True).strip(),"rc")
 def test_runtime_wrong_archive_entry(self):
  d=self.server/"v2.0.0";asset=ASSET.replace("vibe-sdk-","vibe-runtime-")
  shutil.copyfile(d/ASSET,d/asset)
  h=hashlib.sha256((d/asset).read_bytes()).hexdigest();(d/"SHA256SUMS").write_text(f"{h}  {asset}\n")
  r=self.runit("install","--runtime","--version","v2.0.0");self.assertNotEqual(r.returncode,0);self.assertIn("unexpected path",r.stderr)
 def test_legacy_sdk_only(self):
  self.release("v1.0.0-rc.1","legacy");d=self.server/"v1.0.0-rc.1";legacy=ASSET.replace("vibe-sdk-","vibe-")
  (d/ASSET).rename(d/legacy);(d/ASSET.replace("vibe-sdk-","vibe-runtime-")).unlink()
  h=hashlib.sha256((d/legacy).read_bytes()).hexdigest();(d/"SHA256SUMS").write_text(f"{h}  {legacy}\n")
  r=self.runit("install","--version","v1.0.0-rc.1");self.assertEqual(r.returncode,0,r.stderr)
  r=self.runit("install","--runtime","--version","v1.0.0-rc.1");self.assertNotEqual(r.returncode,0);self.assertIn("this release has no vibe-runtime asset for x86_64-unknown-linux-gnu",r.stderr)
 def test_curl_https_only(self):
  t=SCRIPT.read_text();self.assertEqual(t.count("curl -f"),t.count("--proto '=https' --tlsv1.2"))
if __name__=='__main__':unittest.main(verbosity=2)

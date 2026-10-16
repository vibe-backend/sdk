#!/usr/bin/env python3
import hashlib,os,pathlib,shutil,subprocess,tarfile,tempfile,unittest
ROOT=pathlib.Path(__file__).resolve().parents[1]; SCRIPT=ROOT/"scripts/vibe-install.sh"; ASSET="vibe-x86_64-unknown-linux-gnu.tar.gz"
class T(unittest.TestCase):
 def setUp(self):
  self.t=pathlib.Path(tempfile.mkdtemp()); self.server=self.t/"server"; self.cmd=self.t/"cmd"; self.dest=self.t/"install"; self.server.mkdir(); self.cmd.mkdir()
  self.command("uname",'#!/bin/sh\n[ "$1" = "-s" ] && echo Linux || echo x86_64\n'); self.command("gh",'#!/bin/sh\nprintf "%s\\n" "$*" >> "$MOCK_GH_LOG"\nif [ "$1" = api ]; then echo "${MOCK_SHA:-0123456789abcdef0123456789abcdef01234567}"; exit 0; fi\n[ "${MOCK_ATTEST_FAIL:-0}" != 1 ]\n')
  self.command("curl",'''#!/bin/sh
set -eu
out=; url=
while [ $# -gt 0 ]; do case $1 in -o) out=$2;shift 2;;-w|--retry|--proto)shift 2;;--tlsv1.2|-f|-L|-s|-S|-I|-fsSIL)shift;;*)url=$1;shift;;esac;done
if echo "$url"|grep -q '/releases/latest$';then printf '%s/test/repo/releases/tag/%s' "$VIBE_INSTALL_DOWNLOAD_URL" "${MOCK_LATEST:-v2.0.0}";exit 0;fi
[ "${MOCK_NETWORK_FAIL:-0}" != 1 ]||exit 22
name=${url##*/};rest=${url#*/download/};version=${rest%%/*};cp "$MOCK_SERVER/$version/$name" "$out"
''')
  self.env=os.environ|{"PATH":f"{self.cmd}:{os.environ['PATH']}","MOCK_SERVER":str(self.server),"MOCK_GH_LOG":str(self.t/"gh.log"),"VIBE_INSTALL_REPOSITORY":"test/repo","VIBE_INSTALL_DOWNLOAD_URL":"https://mock.invalid","VIBE_INSTALL_DIR":str(self.dest)}; self.release("v1.0.0","one");self.release("v2.0.0","two")
 def tearDown(self):shutil.rmtree(self.t)
 def command(self,n,b):p=self.cmd/n;p.write_text(b);p.chmod(0o755)
 def release(self,v,text,bad=False):
  d=self.server/v;d.mkdir();q=self.t/f"p-{v}";q.mkdir();x=q/"vibe";x.write_text(f"#!/bin/sh\necho {text}\n");x.chmod(0o755)
  with tarfile.open(d/ASSET,"w:gz")as f:f.add(x,arcname="vibe")
  h=hashlib.sha256((d/ASSET).read_bytes()).hexdigest();(d/"SHA256SUMS").write_text(f"{'0'*64 if bad else h}  {ASSET}\n")
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
 def test_multiline_version_rejected(self):r=self.runit("install","--version","x\nv1.0.0\ny",env=self.env|{"MOCK_NETWORK_FAIL":"1"});self.assertNotEqual(r.returncode,0);self.assertIn("invalid stable release version",r.stderr);self.assertFalse((self.t/"gh.log").exists())
 def test_curl_https_only(self):
  t=SCRIPT.read_text();self.assertEqual(t.count("curl -f"),t.count("--proto '=https' --tlsv1.2"))
 def two(self):return subprocess.check_output([self.dest/"vibe"],text=True).strip()
 def test_below_minimum_fails(self):
  self.release("v0.0.9","old");r=self.runit("install","--version","v0.0.9",env=self.env|{"VIBE_INSTALL_MIN_VERSION":"v0.0.1"});self.assertNotEqual(r.returncode,0);self.assertIn("below the minimum supported version",r.stderr);self.assertFalse((self.dest/"vibe").exists());self.assertFalse((self.t/"gh.log").exists())
 def test_overridden_latest_older_tag_fails(self):
  self.release("v0.0.5","old");r=self.runit("upgrade",env=self.env|{"MOCK_LATEST":"v0.0.5"});self.assertNotEqual(r.returncode,0);self.assertIn("below the minimum supported version",r.stderr);self.assertFalse((self.dest/"vibe").exists())
 def test_latest_older_than_installed_refused(self):
  self.release("v1.5.0","mid");r=self.runit("install","--version","v1.5.0");self.assertEqual(r.returncode,0,r.stderr);r=self.runit("upgrade",env=self.env|{"MOCK_LATEST":"v1.0.0"});self.assertNotEqual(r.returncode,0);self.assertIn("refusing downgrade",r.stderr);self.assertEqual(self.two(),"mid")
 def test_downgrade_refused_without_flag(self):
  self.assertEqual(self.runit("install","--version","v2.0.0").returncode,0);r=self.runit("install","--version","v1.0.0");self.assertNotEqual(r.returncode,0);self.assertIn("--allow-downgrade",r.stderr);self.assertEqual(self.two(),"two")
  r=self.runit("install","--version","v1.0.0","--allow-downgrade");self.assertEqual(r.returncode,0,r.stderr);self.assertEqual(self.two(),"one")
 def test_allow_downgrade_never_below_minimum(self):
  self.release("v0.0.9","old");self.install1();r=self.runit("install","--version","v0.0.9","--allow-downgrade");self.assertNotEqual(r.returncode,0);self.assertIn("below the minimum",r.stderr);self.assertEqual(self.two(),"one")
 def test_nonsemver_installed_refused(self):
  self.install1();man=next((self.dest/".vibe-managed"/"versions").glob("*/manifest"));man.write_text(man.read_text().replace("version=v1.0.0","version=1"))
  r=self.runit("install","--version","v2.0.0");self.assertNotEqual(r.returncode,0);self.assertIn("non-semver",r.stderr);self.assertEqual(self.two(),"one")
  r=self.runit("install","--version","v2.0.0","--allow-downgrade");self.assertEqual(r.returncode,0,r.stderr);self.assertEqual(self.two(),"two")
  self.release("v0.0.9","old");man=next((self.dest/".vibe-managed"/"versions").glob("*/manifest"));man.write_text(man.read_text().replace("version=v2.0.0","version=1"))
  r=self.runit("install","--version","v0.0.9","--allow-downgrade");self.assertNotEqual(r.returncode,0);self.assertIn("below the minimum",r.stderr);self.assertEqual(self.two(),"two")
 def test_sha256_match_and_mismatch(self):
  h=hashlib.sha256((self.server/"v1.0.0"/ASSET).read_bytes()).hexdigest()
  r=self.runit("install","--version","v1.0.0","--sha256","f"*64);self.assertNotEqual(r.returncode,0);self.assertIn("does not match --sha256",r.stderr);self.assertFalse((self.dest/"vibe").exists())
  self.assertNotEqual(self.runit("install","--version","v1.0.0","--sha256","xyz").returncode,0)
  r=self.runit("install","--version","v1.0.0","--sha256",h.upper());self.assertEqual(r.returncode,0,r.stderr)
 def test_semver_compare(self):
  for a,b,want in [("1.10.0","1.9.0","1"),("v1.9.0","v1.10.0","-1"),("1.0.0-alpha","1.0.0","-1"),("1.0.0","1.0.0-rc.1","1"),("1.0.0-alpha","1.0.0-alpha.1","-1"),("1.0.0-alpha.1","1.0.0-alpha.beta","-1"),("1.0.0-beta.2","1.0.0-beta.11","-1"),("1.0.0-rc.1","1.0.0-beta.11","1"),("v2.0.0","v2.0.0","0"),("0.10.0","0.9.99","1")]:
   r=self.runit("__compare-versions",a,b);self.assertEqual(r.stdout.strip(),want,(a,b))
 def test_minimum_is_hardcoded(self):
  t=SCRIPT.read_text();self.assertRegex(t,r"\nMIN_VERSION=v[0-9]+\.[0-9]+\.[0-9]+\n");self.assertNotIn("MIN_VERSION:-",t);self.assertNotIn("--min-version",t)
if __name__=='__main__':unittest.main(verbosity=2)

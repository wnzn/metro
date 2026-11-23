"""MetroWindowsSigning.psm1: the tampered-MSI editor leaves no open handle behind.

Run 45247199346 failed with "Get-AuthenticodeSignature: The process cannot access
the file ...tampered.msi because it is being used by another process": the editor's
View (and the Installer) still referenced the database. The edit now runs in a
child pwsh that exits before New-TamperedMsiCopy returns, and the caller requires an
exclusive open. These tests run on any host with PowerShell 7; the Windows
Installer edit itself and Authenticode's HashMismatch are checked natively in the
Windows verifier self-test.
"""

import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
MODULE = HERE / "MetroWindowsSigning.psm1"


@unittest.skipUnless(shutil.which("pwsh"), "PowerShell 7 (pwsh) is not installed")
class TamperEditorHandles(unittest.TestCase):
    def ps(self, body, **env):
        # Module-private functions are called inside the module's scope.
        script = ("$ErrorActionPreference = 'Stop'; Import-Module $env:METRO_MODULE; "
                  "& (Get-Module MetroWindowsSigning) { " + body + " }")
        r = subprocess.run(["pwsh", "-NoProfile", "-NonInteractive", "-Command", script],
                           text=True, capture_output=True, timeout=180,
                           env={**os.environ, "METRO_MODULE": str(MODULE), **env})
        # pwsh colours and wraps its error view; compare plain, single-spaced text.
        r.stderr = " ".join(re.sub(r"\x1b\[[0-9;]*m", "", r.stderr).split())
        return r

    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="metro tamper "))
        self.addCleanup(shutil.rmtree, self.dir, True)
        self.file = self.dir / "tampered copy.msi"
        self.file.write_bytes(b"msi bytes")

    def test_exclusive_open_is_required(self):
        # One .NET FileShare.None holder in the same process: the check must fail while
        # it is open (Windows sharing violation; flock on Unix) and pass once disposed.
        body = (
            "$held = [IO.File]::Open($env:F, 'Open', 'Read', 'None'); "
            "try { try { Assert-MetroFileClosed $env:F 'the tampered MSI'; 'PASSED WHILE HELD' } "
            "catch { 'refused: ' + $_.Exception.Message } } finally { $held.Dispose() }; "
            "Assert-MetroFileClosed $env:F 'the tampered MSI'; 'closed after dispose'")
        r = self.ps(body, F=str(self.file))
        self.assertEqual(r.returncode, 0, r.stderr)
        lines = r.stdout.strip().splitlines()
        self.assertEqual(len(lines), 2, r.stdout)
        self.assertTrue(lines[0].startswith(
            "refused: MetroWindowsSigning: the tampered MSI is still open after its editor exited"), lines[0])
        self.assertEqual(lines[1], "closed after dispose")

    def test_child_editor_handles_end_with_the_child(self):
        # The child opens the file exclusively, writes and never disposes the stream:
        # its exit alone must release the handle before the caller continues.
        body = (
            "$b = { param([string] $Path, [string] $Text) "
            "$s = [IO.File]::Open($Path, 'Open', 'ReadWrite', 'None'); "
            "$w = [Text.Encoding]::ASCII.GetBytes($Text); $s.Write($w, 0, $w.Length); $s.Flush() }; "
            "Invoke-MetroChildEditor $b @($env:F, 'edited by child') 'the test editor'; "
            "Assert-MetroFileClosed $env:F 'the edited file'; [IO.File]::ReadAllText($env:F)")
        r = self.ps(body, F=str(self.file))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "edited by child")

    def test_child_editor_failure_is_named(self):
        r = self.ps("Invoke-MetroChildEditor { param($a, $b) throw \"editor saw [$a] [$b]\" } "
                    "@('C:\\x y\\a.msi', 'two words') 'the MSI editor'; 'continued'")
        self.assertNotEqual(r.returncode, 0)
        self.assertNotIn("continued", r.stdout)
        self.assertIn("MetroWindowsSigning: the MSI editor exited with code 1", r.stderr)
        self.assertIn("editor saw [C:\\x y\\a.msi] [two words]", r.stderr)

    def test_tampered_copy_never_passes_without_windows_installer(self):
        if os.name == "nt":
            self.skipTest("Windows Installer is available; the native self-test covers the edit")
        dest = self.dir / "out.msi"
        r = self.ps("New-TamperedMsiCopy $env:F $env:D; 'returned'", F=str(self.file), D=str(dest))
        self.assertNotEqual(r.returncode, 0)
        self.assertNotIn("returned", r.stdout)
        self.assertIn("MetroWindowsSigning: the MSI editor exited with code", r.stderr)


if __name__ == "__main__":
    unittest.main()

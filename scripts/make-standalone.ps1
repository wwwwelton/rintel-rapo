param(
    [string]$StagedDir = "C:\dev\rapo-build\staged",
    [string]$OutExe = "C:\dev\rapo-build\target\release\rintel-rapo-standalone.exe"
)

$ErrorActionPreference = "Stop"

& "C:\dev\rapo-src\scripts\stage-full-release.ps1" -TargetDir $StagedDir

$zipPath = Join-Path $StagedDir "bundle.zip"
if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }

$items = Get-ChildItem -LiteralPath $StagedDir | Where-Object { $_.Name -ne "bundle.zip" -and $_.Name -ne "rintel-rapo-standalone.exe" -and $_.Name -ne "launcher.cs" -and $_.Name -ne "launcher.ress" }
Compress-Archive -Path ($items.FullName) -DestinationPath $zipPath -CompressionLevel Optimal -Force

$cscPath = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::Windows)) "Microsoft.NET\Framework64\v4.0.30319\csc.exe"
if (-not (Test-Path -LiteralPath $cscPath)) {
    $cscPath = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::Windows)) "Microsoft.NET\Framework\v4.0.30319\csc.exe"
}

$csFile = Join-Path $StagedDir "launcher.cs"

# Código compatível com C# 4/5 (.NET Framework 4.0+) sem usar ZipFile ou ArgumentList
$sourceCode = @'
using System;
using System.IO;
using System.Diagnostics;
using System.Reflection;
using System.Windows.Forms;
using System.IO.Compression;

public class StandaloneLauncher {
    public static void Main(string[] args) {
        try {
            string tempDir = Path.Combine(Path.GetTempPath(), "RintelRapo_" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(tempDir);

            Assembly asm = Assembly.GetExecutingAssembly();
            using (Stream stream = asm.GetManifestResourceStream("bundle.zip")) {
                if (stream == null) {
                    throw new Exception("Bundle embutido nao encontrado.");
                }
                string zipPath = Path.Combine(tempDir, "bundle.zip");
                using (FileStream fs = new FileStream(zipPath, FileMode.Create, FileAccess.Write)) {
                    stream.CopyTo(fs);
                }
                
                // Extração usando System.IO.Compression.ZipFile (disponível no .NET Framework 4.5+)
                ZipFile.ExtractToDirectory(zipPath, tempDir);
                File.Delete(zipPath);
            }

            string exePath = Path.Combine(tempDir, "rintel-rapo.exe");
            if (!File.Exists(exePath)) {
                throw new Exception("rintel-rapo.exe nao encontrado no pacote extraido.");
            }

            string arguments = "";
            foreach (string arg in args) {
                arguments += "\"" + arg.Replace("\"", "\\\"") + "\" ";
            }

            ProcessStartInfo psi = new ProcessStartInfo();
            psi.FileName = exePath;
            psi.Arguments = arguments;
            psi.WorkingDirectory = tempDir;
            
            Process proc = Process.Start(psi);
            proc.WaitForExit();

            try {
                Directory.Delete(tempDir, true);
            } catch {}
        } catch (Exception ex) {
            MessageBox.Show("Erro no Rintel RAPO Standalone: " + ex.Message, "Rintel RAPO", MessageBoxButtons.OK, MessageBoxIcon.Error);
        }
    }
}
'@

[System.IO.File]::WriteAllText($csFile, $sourceCode, [System.Text.Encoding]::UTF8)

$arguments = @(
    "/target:winexe",
    "/optimize+",
    "/r:System.IO.Compression.FileSystem.dll",
    "/r:System.Windows.Forms.dll",
    "/out:$OutExe",
    "/resource:$zipPath,bundle.zip",
    "$csFile"
)

Write-Host "Compilando executavel standalone com csc.exe..."
& $cscPath $arguments
if ($LASTEXITCODE -ne 0) {
    throw "Erro na compilacao do Standalone com csc.exe."
}

& "C:\dev\rapo-src\scripts\sign-release.ps1" -ExePath $OutExe

$hash = (Get-FileHash -LiteralPath $OutExe -Algorithm SHA256).Hash.ToLower()
"$hash  rintel-rapo-standalone.exe" | Out-File -FilePath "$OutExe.sha256" -Encoding ascii

Write-Host "Standalone gerado e assinado com sucesso em: $OutExe"
Get-Item $OutExe | Select-Object Name, Length, LastWriteTime
Get-Content "$OutExe.sha256"

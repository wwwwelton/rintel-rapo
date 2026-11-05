param(
    [string]$StagedDir = "C:\dev\rapo-build\staged",
    [string]$OutExe = "C:\dev\rapo-build\target\release\rintel-rapo-standalone.exe"
)

$ErrorActionPreference = "Stop"

& "C:\dev\rapo-src\scripts\stage-full-release.ps1" -TargetDir $StagedDir

$zipPath = Join-Path $StagedDir "bundle.zip"
if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }

$items = Get-ChildItem -LiteralPath $StagedDir | Where-Object { 
    $_.Name -ne "bundle.zip" -and 
    $_.Name -ne "rintel-rapo-standalone.exe" -and 
    $_.Name -ne "launcher.cs" -and 
    $_.Name -ne "launcher.ress" 
}
Compress-Archive -Path ($items.FullName) -DestinationPath $zipPath -CompressionLevel Optimal -Force

$cscPath = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::Windows)) "Microsoft.NET\Framework64\v4.0.30319\csc.exe"
if (-not (Test-Path -LiteralPath $cscPath)) {
    $cscPath = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::Windows)) "Microsoft.NET\Framework\v4.0.30319\csc.exe"
}

$csFile = Join-Path $StagedDir "launcher.cs"

# Código C# com Auto-Update via GitHub Releases integradas ao Launcher Standalone
$sourceCode = @'
using System;
using System.IO;
using System.IO.Compression;
using System.Diagnostics;
using System.Reflection;
using System.Windows.Forms;
using System.Net;
using System.Text;

public class StandaloneLauncher {
    private const string CurrentVersion = "v0.2.1";
    private const string GithubRepo = "wwwwelton/rintel-rapo";

    public static void Main(string[] args) {
        try {
            // 1. Verificação silenciosa de atualização no GitHub (timeout de 3 segundos)
            try {
                ServicePointManager.SecurityProtocol = SecurityProtocolType.Tls12;
                string apiUrl = "https://api.github.com/repos/" + GithubRepo + "/releases/latest";
                HttpWebRequest request = (HttpWebRequest)WebRequest.Create(apiUrl);
                request.UserAgent = "RintelRapo-AutoUpdater";
                request.Timeout = 3000;

                using (HttpWebResponse response = (HttpWebResponse)request.GetResponse()) {
                    using (StreamReader reader = new StreamReader(response.GetResponseStream())) {
                        string json = reader.ReadToEnd();
                        int tagIndex = json.IndexOf("tag_name");
                        if (tagIndex != -1) {
                            int start = json.IndexOf("\"", tagIndex + 10) + 1;
                            int end = json.IndexOf("\"", start);
                            string latestVersion = json.Substring(start, end - start);

                            if (latestVersion != CurrentVersion) {
                                DialogResult dr = MessageBox.Show(
                                    "Uma nova versão (" + latestVersion + ") do Rintel RAPO está disponível.\nDeseja baixar e atualizar agora?",
                                    "Atualização Disponível",
                                    MessageBoxButtons.YesNo,
                                    MessageBoxIcon.Information
                                );

                                if (dr == DialogResult.Yes) {
                                    PerformUpdate(latestVersion);
                                    return;
                                }
                            }
                        }
                    }
                }
            } catch {
                // Modo offline ou falha na rede: prossegue normalmente com a versão local
            }

            // 2. Extração e Execução Padrão
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

    private static void PerformUpdate(string version) {
        try {
            string downloadUrl = "https://github.com/" + GithubRepo + "/releases/download/" + version + "/rintel-rapo-standalone.exe";
            string currentExe = Assembly.GetExecutingAssembly().Location;
            string tempUpdatePath = Path.Combine(Path.GetTempPath(), "rintel-rapo-update.exe");

            using (WebClient client = new WebClient()) {
                client.DownloadFile(downloadUrl, tempUpdatePath);
            }

            string batchPath = Path.Combine(Path.GetTempPath(), "update.bat");
            string batchContent = 
                "@echo off\r\n" +
                "timeout /t 2 /nobreak > nul\r\n" +
                "move /y \"" + tempUpdatePath + "\" \"" + currentExe + "\"\r\n" +
                "start \"\" \"" + currentExe + "\"\r\n" +
                "del \"%~f0\"\r\n";

            File.WriteAllText(batchPath, batchContent, Encoding.ASCII);

            Process.Start(new ProcessStartInfo {
                FileName = batchPath,
                CreateNoWindow = true,
                UseShellExecute = false
            });

            Environment.Exit(0);
        } catch (Exception ex) {
            MessageBox.Show("Falha ao realizar atualização: " + ex.Message, "Erro de Atualização", MessageBoxButtons.OK, MessageBoxIcon.Error);
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

Write-Host "Compilando executavel standalone com Auto-Update..."
& $cscPath $arguments
if ($LASTEXITCODE -ne 0) {
    throw "Erro na compilacao do Standalone com Auto-Update."
}

& "C:\dev\rapo-src\scripts\sign-release.ps1" -ExePath $OutExe

$hash = (Get-FileHash -LiteralPath $OutExe -Algorithm SHA256).Hash.ToLower()
"$hash  rintel-rapo-standalone.exe" | Out-File -FilePath "$OutExe.sha256" -Encoding ascii

Write-Host "Standalone com Auto-Update gerado e assinado com sucesso em: $OutExe"
Get-Item $OutExe | Select-Object Name, Length, LastWriteTime
Get-Content "$OutExe.sha256"

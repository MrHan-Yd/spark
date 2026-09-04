# Smoke: connect and print plugin list (ASCII only).
$pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'spark.host.ipc', [System.IO.Pipes.PipeDirection]::InOut)
$pipe.Connect(3000)
$enc = New-Object System.Text.UTF8Encoding($false)
$writer = New-Object System.IO.StreamWriter($pipe, $enc, 65536)
$writer.AutoFlush = $true
$writer.NewLine = "`n"
$reader = New-Object System.IO.StreamReader($pipe, [System.Text.Encoding]::UTF8, $false, 65536, $true)
$writer.WriteLine('{"jsonrpc":"2.0","id":1,"method":"host.plugin.list","params":{}}')
$t = $reader.ReadLineAsync()
if (-not $t.Wait([TimeSpan]::FromSeconds(10))) { throw 'list timeout' }
Write-Output $t.Result
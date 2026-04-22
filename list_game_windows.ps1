Get-Process | Where-Object { $_.MainWindowTitle -ne "" } | Where-Object { $_.MainWindowTitle -match 'game|Game|test|Test|d3d|D3D' } | Select-Object ProcessName, MainWindowTitle, Id

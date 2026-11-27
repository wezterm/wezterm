param(
  [Parameter(Mandatory = $true)]
  [int] $TargetProcessId,
  [switch] $Activate,
  [string] $InsertText
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class AccessibilitySmokeWindow {
  [DllImport("user32.dll")]
  public static extern bool SetForegroundWindow(IntPtr window);
}
'@

$process = Get-Process -Id $TargetProcessId
if ($process.ProcessName -ne 'wezterm-gui') {
  throw 'The target must be a dedicated wezterm-gui test process.'
}
$window = $process.MainWindowHandle
if ($window -eq [IntPtr]::Zero) { throw 'The target has no window.' }
if ($Activate) {
  [void][AccessibilitySmokeWindow]::SetForegroundWindow($window)
  Start-Sleep -Milliseconds 250
}

$root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
$condition = New-Object System.Windows.Automation.PropertyCondition(
  [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
  [System.Windows.Automation.ControlType]::Edit
)
$inputElement = $root.FindFirst(
  [System.Windows.Automation.TreeScope]::Descendants, $condition
)
if ($null -eq $inputElement) { throw 'No editable input element was exposed.' }
$value = $inputElement.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
$text = $inputElement.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
$selection = $text.GetSelection()
$readOnly = $text.DocumentRange.GetAttributeValue(
  [System.Windows.Automation.TextPattern]::IsReadOnlyAttribute
)
$result = [ordered]@{
  ProcessId = $TargetProcessId
  Name = $inputElement.Current.Name
  ControlType = $inputElement.Current.ControlType.ProgrammaticName
  KeyboardFocusable = $inputElement.Current.IsKeyboardFocusable
  HasKeyboardFocus = $inputElement.Current.HasKeyboardFocus
  Enabled = $inputElement.Current.IsEnabled
  ValueReadOnly = $value.Current.IsReadOnly
  TextReadOnly = $readOnly
  ValueLength = $value.Current.Value.Length
  DocumentLength = $text.DocumentRange.GetText(-1).Length
  SelectionCount = $selection.Length
  Bounds = $inputElement.Current.BoundingRectangle.ToString()
}
if ($result.ValueReadOnly -or $result.TextReadOnly -or !$result.KeyboardFocusable) {
  throw 'The input is not writable and focusable.'
}
if ($result.ValueLength -ne 0 -or $result.DocumentLength -ne 0 -or $result.SelectionCount -ne 1) {
  throw 'Unexpected pending-input buffer or selection.'
}
if ($Activate -and !$result.HasKeyboardFocus) {
  throw 'The test window did not acquire keyboard focus.'
}

# 仅对专用测试进程显式传入文本；测试终端应运行收集器，避免执行命令。
if ($PSBoundParameters.ContainsKey('InsertText')) {
  if (!$result.HasKeyboardFocus) { throw 'Refusing to insert into an unfocused window.' }
  $value.SetValue($InsertText)
  $result.InsertedCharacters = $InsertText.Length
}
$result | ConvertTo-Json

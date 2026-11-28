<#
  Static check for scripts that run under Windows PowerShell 5.1
  (powershell.exe on .NET Framework): reports PowerShell 7 syntax,
  cmdlets, parameters, variables and .NET Core-only APIs. Parses with the
  pwsh 7 parser (a superset of 5.1's grammar). One line per finding,
  "<file>:<line>: <what>"; exit 1 if any. Used by packaging tests.
#>
param([Parameter(Mandatory = $true, ValueFromRemainingArguments = $true)] [string[]] $Path)
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$L = [System.Management.Automation.Language.Ast]
$commands = 'Test-Json', 'Get-Error', 'Join-String', 'ConvertFrom-Markdown', 'Show-Markdown', 'Get-Uptime', 'Remove-Alias',
    'Remove-Service', 'Get-MarkdownOption', 'Set-MarkdownOption', 'Start-ThreadJob', 'Update-Help2'
$parameters = 'Parallel', 'AsByteStream', 'AsHashtable', 'NoProxy', 'SkipCertificateCheck', 'AdditionalChildPath', 'LeafBase',
    'SkipHttpErrorCheck', 'StatusCodeVariable', 'ResponseHeadersVariable', 'AllowInsecureRedirect', 'NoEmphasis', 'EnumsAsStrings',
    'AsArray', 'DateKind', 'MaximumRetryCount', 'RetryIntervalSec', 'Resume', 'SkipHeaderValidation'
$variables = 'IsWindows', 'IsLinux', 'IsMacOS', 'IsCoreCLR', 'PSStyle', 'PSNativeCommandArgumentPassing', 'PSNativeCommandUseErrorActionPreference'
# Static .NET members missing from .NET Framework 4.x (as Type::Member).
$members = 'File::Move/3', 'Path::GetRelativePath', 'Path::Join', 'Path::TrimEndingDirectorySeparator', 'Path::EndsInDirectorySeparator',
    'Convert::ToHexString', 'Convert::FromHexString', 'Math::Clamp', 'Encoding::Latin1', 'File::ReadAllTextAsync',
    'File::WriteAllTextAsync', 'Directory::CreateSymbolicLink', 'File::CreateSymbolicLink', 'RandomNumberGenerator::Fill',
    'RandomNumberGenerator::GetBytes', 'RandomNumberGenerator::GetInt32', 'SHA256::HashData', 'OperatingSystem::IsWindows',
    'Environment::ProcessPath', 'Environment::ProcessId'
$found = [Collections.Generic.List[string]]::new()
foreach ($file in $Path) {
    $tokens = $null; $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($file, [ref] $tokens, [ref] $errors)
    foreach ($e in $errors) { $found.Add("${file}:$($e.Extent.StartLineNumber): parse error: $($e.Message)") }
    $add = { param($n, $what) $found.Add("${file}:$($n.Extent.StartLineNumber): $what") }
    foreach ($n in $ast.FindAll({ $true }, $true)) {
        switch ($n.GetType().Name) {
            'TernaryExpressionAst' { & $add $n 'ternary ?: (7.0+)' }
            'PipelineChainAst' { & $add $n 'pipeline chain && / || (7.0+)' }
            'BinaryExpressionAst' { if ($n.Operator -eq 'QuestionQuestion') { & $add $n '?? (7.0+)' } }
            'AssignmentStatementAst' { if ($n.Operator -eq 'QuestionQuestionEquals') { & $add $n '??= (7.0+)' } }
            'MemberExpressionAst' {
                if ($n.NullConditional) { & $add $n '?. (7.1+)' }
            }
            'InvokeMemberExpressionAst' {
                if ($n.NullConditional) { & $add $n '?. (7.1+)' }
                if ($n.Static -and $n.Expression -is [System.Management.Automation.Language.TypeExpressionAst] -and $n.Member -is [System.Management.Automation.Language.StringConstantExpressionAst]) {
                    $type = $n.Expression.TypeName.Name -replace '^.*\.', ''
                    $m = $n.Member.Value
                    $argc = if ($n.Arguments) { $n.Arguments.Count } else { 0 }
                    if ("${type}::$m" -in $members -or "${type}::$m/$argc" -in $members) { & $add $n "[$type]::$m($argc args) is .NET Core only" }
                }
            }
            'IndexExpressionAst' { if ($n.NullConditional) { & $add $n '?[] (7.1+)' } }
            'CommandAst' {
                $name = $n.GetCommandName()
                if ($name -and $name -in $commands) { & $add $n "cmdlet $name (7.x)" }
                $isEncoding = $false
                foreach ($el in $n.CommandElements) {
                    if ($el -is [System.Management.Automation.Language.CommandParameterAst]) {
                        if ($el.ParameterName -in $parameters) { & $add $el "-$($el.ParameterName) (7.x)" }
                        $isEncoding = $el.ParameterName -eq 'Encoding'
                    } elseif ($isEncoding -and $el -is [System.Management.Automation.Language.StringConstantExpressionAst] -and $el.Value -in 'utf8NoBOM', 'utf8BOM', 'ansi') {
                        & $add $el "-Encoding $($el.Value) (7.x)"; $isEncoding = $false
                    } else { $isEncoding = $false }
                }
            }
            'VariableExpressionAst' { if ($n.VariablePath.UserPath -in $variables) { & $add $n "`$$($n.VariablePath.UserPath) (7.x)" } }
        }
    }
    # The clean {} block (7.3+).
    if ($ast.PSObject.Properties['CleanBlock'] -and $ast.CleanBlock) { & $add $ast.CleanBlock 'clean {} block (7.3+)' }
}
$found | Sort-Object -Unique | ForEach-Object { Write-Host $_ }
if ($found.Count) { exit 1 }
exit 0

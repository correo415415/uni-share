// Plantilla de reglas YARA para uni-share.
// Copia este archivo (o los tuyos) a <data_dir>/rules/ — p. ej. ~/.config/uni-share/rules/ en Linux,
// %APPDATA%\uni-share\rules\ en Windows — y activa «Usar reglas YARA» en Ajustes → Seguridad.
// Cada archivo se compila en su propio namespace (el nombre del archivo sin extensión).
//
// Severidad de cada regla (por defecto: peligro):
//   meta: severity = "info" | "low"                        → información
//   meta: severity = "warning" | "medium" | "suspicious"   → aviso
//   meta: severity = "danger" | "high" | "critical" | "malware" → peligro (cuarentena si está configurada)
// También vale una etiqueta:  rule nombre : suspicious { … }
// meta: description se añade al mensaje mostrado.

rule eicar_test_file
{
    meta:
        severity = "high"
        description = "Archivo de prueba EICAR (no es malware real, sirve para comprobar el motor)"
    strings:
        $eicar = "X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*"
    condition:
        $eicar
}

rule office_macro_autoexec : suspicious
{
    meta:
        description = "Documento Office con macro que se ejecuta al abrir"
    strings:
        $a = "AutoOpen" ascii wide nocase
        $b = "Document_Open" ascii wide nocase
        $c = "Workbook_Open" ascii wide nocase
        $vba = "vbaProject.bin" ascii
    condition:
        $vba and any of ($a, $b, $c)
}

rule powershell_encoded_command
{
    meta:
        severity = "medium"
        description = "Llamada a PowerShell con comando codificado en Base64"
    strings:
        $ps = "powershell" ascii wide nocase
        $enc1 = "-EncodedCommand" ascii wide nocase
        $enc2 = /-e(nc?)?\s+[A-Za-z0-9+\/=]{40,}/ ascii wide nocase
    condition:
        $ps and any of ($enc*)
}

rule large_pe_with_upx : info
{
    meta:
        description = "Ejecutable Windows empaquetado con UPX (no necesariamente malicioso)"
    strings:
        $upx0 = "UPX0"
        $upx1 = "UPX1"
    condition:
        uint16(0) == 0x5A4D and all of them
}

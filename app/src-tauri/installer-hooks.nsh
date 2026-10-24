; Seam installer hooks.
;
; Allow Seam through Windows Firewall on private and domain networks only.
; On public networks (cafes, airports) Windows keeps blocking it, which is
; what you want there.

!macro NSIS_HOOK_POSTINSTALL
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Seam"'
  nsExec::Exec 'netsh advfirewall firewall add rule name="Seam" dir=in action=allow program="$INSTDIR\Seam.exe" protocol=UDP profile=private,domain enable=yes description="Seam: share mouse, keyboard and clipboard with your other PCs"'
  CreateShortcut "$SENDTO\Seam (other PC).lnk" "$INSTDIR\Seam.exe" "--send" "$INSTDIR\Seam.exe" 0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Seam"'
  nsExec::Exec 'schtasks /Delete /TN "Seam" /F'
  Delete "$SENDTO\Seam (other PC).lnk"
!macroend

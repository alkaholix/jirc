; ============================================================================
; i7updater.mrc -- native jIRC IRC7 Passport updater for i7.mrc
;
; Uses jIRC's built-in WebView support. No nHTMLn.dll, WebView2Loader.dll,
; HWND plumbing, or other browser DLL is required.
;
;   /i7update <name>          reuse the current WebView sign-in session
;   /i7update <name> pick [client|bot]
;                             sign out, choose an account, optionally select it
;   /i7update <name> auto client|bot
;   /i7update status
;   /i7upauto on|off          23-hour automatic refresh (default on)
; Selected registered client/b7 Passports are checked once after startup.
;
; Cookies are staged in memory. Existing working credentials are replaced only
; after both ticket and profile have been captured. Passwords are never stored.
; ============================================================================

alias -l i7up_ini { return $qt($+($scriptdir,settings.ini)) }
alias -l i7up_view { return i7update }
alias -l i7up_login { return https://api.irc7.com/api/auth/login }
alias -l i7up_return { return https://www.irc7.com }
alias -l i7up_logout { return https://login.live.com/logout.srf }

alias i7update {
  if ($lower($1) == status) {
    var %view = $i7up_view
    echo -st * i7update: $iif($webview(%view),running for $hget(i7update,name) $+($chr(40),$webview(%view).status,$chr(41)),idle) $+ , auto-refresh $iif($readini($i7up_ini,n,settings,autoupdate) == on,ON,OFF)
    return
  }
  if (!$1) { echo -st * i7update: usage /i7update <name> [pick] | return }

  i7up_close
  if ($lower($2) == auto) set %i7n.authupdating 1
  hmake i7update 20
  hadd i7update name $1
  hadd i7update pick $iif($lower($2) == pick,1,0)
  if (($lower($2) == pick) && ($istok(client bot,$lower($3),32))) hadd i7update target $lower($3)
  if ($lower($2) == auto) hadd i7update restart $lower($3)
  if ($lower($2) == startup) hadd i7update startup 1
  hadd i7update state opening

  ; Every Passport gets its own persistent, isolated browser profile so client
  ; and bot Microsoft sessions cannot cross-contaminate one another's tickets.
  var %profile = $i7up_profkey($hget(i7update,name))
  webview -o $i7up_view %profile 980 720 about:blank IRC7 Passport Update - $1
  .timeri7uptimeout 1 300 i7up_timeout
  echo -st * i7update: opening secure sign-in for " $+ $1 $+ "
}

alias -l i7up_login_now {
  if (!$webview($i7up_view)) return
  hadd i7update state signing-in
  var %url = $+($i7up_login,?returnUrl=,$i7up_urlenc($i7up_return))
  webview -n $i7up_view %url
}

; jIRC raises this event for browser lifecycle, navigation, and cookie results.
on *:WEBVIEW:i7update:{
  var %event = $lower($1)

  if (%event == closed) {
    ; Ignore a delayed close notification if another updater WebView with the
    ; same name has already replaced the old one.
    if (!$webview($i7up_view)) i7up_cleanup
    return
  }
  if (!$hget(i7update)) return

  if (%event == opened) {
    webview -f $i7up_view
    if ($hget(i7update,pick) == 1) {
      hadd i7update state signing-out
      webview -n $i7up_view $i7up_logout
      .timeri7uplogin 1 3 i7up_login_now
    }
    else i7up_login_now
  }
  elseif (%event == navigate_complete) {
    var %url = $2-
    ; Capture only after IRC7 has completed the Microsoft callback and returned
    ; to its own site. Microsoft login pages are deliberately ignored.
    if (($pos(%url,https://www.irc7.com) == 1) || ($pos(%url,https://irc7.com) == 1)) {
      if (!$hget(i7update,capturing)) {
        hadd i7update capturing 1
        hadd i7update state reading-cookies
        hadd i7update cookietries 1
        webview -k $i7up_view $i7up_return
      }
    }
  }
  elseif (%event == cookie) {
    var %name = $lower($2), %cookie = $3-
    if ($istok(ticket profile regcookie puid email,%name,32)) hadd i7update %name $i7up_decode(%cookie)
  }
  elseif (%event == cookies_done) i7up_finish
  elseif (%event == error) {
    echo -st * i7update: WebView error: $2-
    i7up_close
  }
}

alias -l i7up_finish {
  var %ticket = $hget(i7update,ticket), %profile = $hget(i7update,profile)
  if ((!$len(%ticket)) || (!$len(%profile))) {
    if ($hget(i7update,cookietries) < 3) {
      hinc i7update cookietries
      hadd i7update state retrying-cookies
      .timeri7upcookies -m 1 1000 i7up_retrycookies
      return
    }
    echo -st * i7update: IRC7 returned without a complete ticket/profile -- try /i7update $hget(i7update,name) pick
    i7up_close
    return
  }

  var %name = $hget(i7update,name), %section = pp. $+ %name, %restart = $hget(i7update,restart), %startup = $hget(i7update,startup), %target = $hget(i7update,target), %email = $hget(i7update,email)
  ; A non-interactive refresh captures whichever account is already signed in
  ; to this Passport's isolated profile. Refuse to overwrite a Passport if that
  ; identity no longer matches; only explicit "pick" may change its account.
  if ($hget(i7update,pick) != 1) {
    var %oldpuid = $readini($i7up_ini,n,%section,puid), %newpuid = $hget(i7update,puid)
    var %oldemail = $lower($readini($i7up_ini,n,%section,email)), %newemail = $lower(%email)
    var %mismatch = 0
    if (($len(%oldpuid)) && ($len(%newpuid))) { if (%oldpuid != %newpuid) var %mismatch = 1 }
    elseif (($len(%oldemail)) && ($len(%newemail)) && (%oldemail != %newemail)) var %mismatch = 1
    if (%mismatch) {
      echo -st * i7update: WebView is signed in as $iif(%newemail,%newemail,a different account) but Passport " $+ %name $+ " belongs to $iif(%oldemail,%oldemail,another account) -- NOT overwriting. Run /i7update %name pick to sign in as the correct account.
      i7up_close
      return
    }
  }

  ; With no explicit pick target, select this Passport only when exactly one
  ; registered role is missing a selection. Existing selections are untouched,
  ; and an ambiguous client+bot gap is left for the user to choose explicitly.
  if (%target == $null) {
    var %needclient = 0, %needbot = 0
    if (($lower($readini($i7up_ini,n,settings,mode)) == registered) && ($readini($i7up_ini,n,settings,curpp) == $null)) var %needclient = 1
    if (($lower($readini($i7up_ini,n,settings,botmode)) == registered) && ($readini($i7up_ini,n,settings,botpp) == $null)) var %needbot = 1
    if (%needclient != %needbot) var %target = $iif(%needclient,client,bot)
  }

  var %oldticket = $readini($i7up_ini,n,%section,ticket), %oldprofile = $readini($i7up_ini,n,%section,profile)
  ; Preserve one known-good previous pair before committing the complete new pair.
  if (($len(%oldticket)) && ($len(%oldprofile)) && ((%oldticket != %ticket) || (%oldprofile != %profile))) {
    writeini -n $i7up_ini %section previous_ticket %oldticket
    writeini -n $i7up_ini %section previous_profile %oldprofile
  }
  writeini -n $i7up_ini %section ticket %ticket
  writeini -n $i7up_ini %section profile %profile
  if ($len($hget(i7update,regcookie))) writeini -n $i7up_ini %section regcookie $hget(i7update,regcookie)
  if ($len($hget(i7update,puid))) writeini -n $i7up_ini %section puid $hget(i7update,puid)
  if ($len($hget(i7update,email))) writeini -n $i7up_ini %section email $hget(i7update,email)
  writeini -n $i7up_ini %section lastupdate $ctime
  if (%target == client) writeini -n $i7up_ini settings curpp %name
  elseif (%target == bot) writeini -n $i7up_ini settings botpp %name
  echo -st * i7update: Passport " $+ %name $+ " updated successfully $iif(%email,$+($chr(40),%email,$chr(41)),)
  if (%target != $null) echo -st * i7update: Passport " $+ %name $+ " selected for %target
  else echo -st * i7update: Passport " $+ %name $+ " saved but not selected -- use /i7pp use %name for client or /i7pp bot %name for bot
  i7up_close
  if ((%restart != $null) || (%i7n.authclientwait) || (%i7n.authbotwait)) signal -n i7authupdated
  if (%startup) .timeri7upstartup -m 1 1000 i7up_startcheck
}

alias -l i7up_retrycookies {
  if (!$webview($i7up_view)) return
  webview -k $i7up_view $i7up_return
}

alias -l i7up_timeout { echo -st * i7update: timed out waiting for sign-in | i7up_close }

alias -l i7up_cleanup {
  .timeri7uplogin off
  .timeri7upcookies off
  .timeri7uptimeout off
  if ($hget(i7update)) hfree i7update
  unset %i7n.authupdating
}

alias -l i7up_close {
  if ($webview($i7up_view)) webview -c $i7up_view
  i7up_cleanup
}

alias i7upauto {
  if (!$istok(on off,$lower($1),32)) { echo -st * i7update: auto-refresh is $iif($readini($i7up_ini,n,settings,autoupdate) == on,ON,OFF) -- /i7upauto on|off | return }
  writeini -n $i7up_ini settings autoupdate $lower($1)
  if ($lower($1) == on) { .timeri7upauto 0 300 i7up_check | echo -st * i7update: auto-refresh ON }
  else { .timeri7upauto off | echo -st * i7update: auto-refresh OFF }
}

alias -l i7up_check {
  if (($webview($i7up_view)) || ($readini($i7up_ini,n,settings,autoupdate) != on)) return
  i7up_startcheck
}

alias -l i7up_startcheck {
  if ($webview($i7up_view)) { .timeri7upstartup 1 10 i7up_startcheck | return }
  var %client = $readini($i7up_ini,n,settings,curpp), %bot = $readini($i7up_ini,n,settings,botpp)
  if (($lower($readini($i7up_ini,n,settings,mode)) == registered) && (%client != $null) && ($i7up_stale(%client))) {
    echo -st * i7update: client Passport " $+ %client $+ " needs updating
    i7update %client startup
    return
  }
  if (($lower($readini($i7up_ini,n,settings,botmode)) == registered) && (%bot != $null) && ($i7up_stale(%bot))) {
    echo -st * i7update: b7 Passport " $+ %bot $+ " needs updating
    i7update %bot startup
  }
}

alias -l i7up_stale {
  if ((!$len($readini($i7up_ini,n,pp. $+ $1,ticket))) || (!$len($readini($i7up_ini,n,pp. $+ $1,profile)))) return $true
  var %updated = $readini($i7up_ini,n,pp. $+ $1,lastupdate)
  if (%updated == $null) return $true
  return $iif($calc($ctime - %updated) >= 82800,$true,$false)
}

; Passport name -> a valid native WebView profile name (letters/digits and
; . _ - only, 1-64 chars, no leading dot).
alias -l i7up_profkey {
  var %k = $regsubex($lower($1),/[^a-z0-9._-]/g,)
  if ($left(%k,1) == .) var %k = p $+ %k
  if (%k == $null) var %k = pp
  return $left(%k,64)
}

alias -l i7up_urlenc { return $regsubex($1-,/([^A-Za-z0-9._~-])/g,$+(%,$base($asc(\t),10,16,2))) }
alias -l i7up_decode { return $regsubex($1-,/%([0-9A-Fa-f]{2})/g,$chr($base(\1,16,10))) }

on *:START:{
  if ($readini($i7up_ini,n,settings,autoupdate) == $null) writeini -n $i7up_ini settings autoupdate on
  .timeri7upstartup 1 5 i7up_startcheck
  if ($readini($i7up_ini,n,settings,autoupdate) == on) .timeri7upauto 0 300 i7up_check
}

on *:LOAD:{
  if ($readini($i7up_ini,n,settings,autoupdate) == $null) writeini -n $i7up_ini settings autoupdate on
  .timeri7upstartup 1 5 i7up_startcheck
  if ($readini($i7up_ini,n,settings,autoupdate) == on) .timeri7upauto 0 300 i7up_check
}

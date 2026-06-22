/*

Name: BV2
Version: 0.31
Author: napalm
Description: A Buzzen v2 (FlashIRCwx) Connection 
Updated Lookup Servers to irc.buzzen.net [d33j4y] 5/2/10

*/

alias BV2 {
  if (!%BV2.info) BV2.login
  if ($sock(BV2*)) sockclose BV2*
  socklisten BV2.start
  .server $iif($1 == -m,$1) localhost $sock(BV2.start).port
}

alias BV2.sock sockopen BV2 irc.buzzen.net 6667

alias BV2.login {
  var %n $$input(FlashIRC Email?,e,Buzzen v2 BETA Login) 
  var %p $md5($$input(FlashIRC Password?,ep,Buzzen v2 BETA Login))
  set %BV2.info %n %p
}

alias BV2.nick { 
  sockclose BV2* | disconnect 
  nick $$1 | BV2
  .timer 1 1 echo $color(kick) -st * Nick Auto Changed From: $2 To: $1
}

on 1:socklisten:BV2.start:{
  sockaccept BV2.local
  sockclose $sockname
  sockwrite -n BV2.local :BuzzenV2 001 $me $+(:,$timestamp) Localhost: Connected.
}

on 1:sockopen:BV2:{ tokenize 32 %BV2.info
  sockwrite -n $sockname AUTHTYPE ircwx1
  sockwrite -n $sockname LOGINH $1 $2
  sockwrite -n $sockname USER $1 * * $replace(:IRC YSNG,I,B,R,V,C,2,Y,v,S,0.,N,3,G,1)
}

on 1:sockread:BV2:{
  var %r | sockread %r | tokenize 32 %r
  if ($sockerr) { 
    echo $color(info2) -st BV2: Error. $sock($sockname).wsmsg 
    bv2 | .timerBV2.recon 0 300 bv2 
    return
  }
  if ($2-3 == NOTICE AUTH) && ($regex($4-,bad password) || $regex($4-,user not found)) {
    sockclose BV2* | disconnect | linesep -s 
    echo $color(kick) -st * User Name and/or Password is incorrect.
  }
  elseif ($2 == 001) {
    if ($me != $regsubex($10,(.+)!.+@.+,\1)) BV2.nick $v2 $me
    elseif ($timer(BV2.recon)) .timerBV2.recon off
  }
  elseif ($2 == PRIVMSG) sockwrite -n BV2.local $regsubex(%r,/\[style.+\](.+)\[/style\]/i,\1)
  elseif ($2 == WHISPER) { 
    sockwrite -n BV2.local $1 PRIVMSG $regsubex($4-,/\[style.+\](.+)\[/style\]/i,\1)
    return
  }
  elseif ($2 == DATA) { 
    var %n = $regsubex($1,:(.+)!.+@.+,\1)
    if ($6 == :WHISPACCEPTNEEDED) echo $color(notice) -t %n * Waiting for %n to Accept Whisper.
    elseif ($6 == :WHISPACCEPTED) echo $color(notice) -t %n * %n has Accepted Your Whisper.
    elseif ($6 == :WHISPDECLINED) echo $color(kick) -t %n * %n has Declined Your Whisper.
    elseif ($6 == :WHISPWNDCLOSED) echo $color(kick) -t %n * %n has Closed the Whisper Window.
  }
  elseif ($2 == 818) echo $color(info) -t $4 * $5-
  elseif ($2 isin 801 802 803 804 805 820) { echo -s $1-
    if ($window(@BV2.access)) && ($2 == 804) { aline @BV2.access $5 $chr(9) $ial($+(*,$6,*)).nick $chr(9) $6 | return }
    elseif ($window(@BV2.access)) && ($2 == 805) { BV2.access.buf 2 | return }
    if ($window(@BV2.access)) return
    elseif ($2 == 820) echo $color(info) -t $4 * Access $iif($5 == *,$null $+,$5) Cleared.
    elseif ($2 == 801) echo $color(info) -t $4 * Access $5 Added: $6
    elseif ($2 == 802) echo $color(info) -t $4 * Access $5 Deleted: $6
    elseif ($2 == 803) echo $color(info) -t $4 * Access List Start
    elseif ($2 == 804) echo $color(info) -t $4 * $5 $iif($ial($+(*,$6,*)).nick,$+ - $v1) - $6
    elseif ($2 == 805) echo $color(info) -t $4 * Access List End
  }
  elseif ($2 == 329) echo $color(topic) -t $4 * Room Created on $asctime($5,dddd mmm ddoo yyyy h:nn tt)
  elseif ($2 == 403) && ($regex($4,$+($chr(37),#.+))) CREATE $4
  else sockwrite -n BV2.local %r
}

on 1:sockread:BV2.local:{
  var %r | sockread %r | tokenize 32 %r
  if ($1 == USER) BV2.sock
  elseif ($1 == PRIVMSG) {
    if ($numtok($2,44) > 1) && ($regex($2,(%#.+))) {
      var %x 1 | while ($gettok($2,%x,44)) { sockwrite -n BV2 $1 $ifmatch $3- | inc %x }
    }
    elseif ($regex($2,(%#.+))) { 
      if (%BV2.text) { 
        sockwrite -n BV2 $1-2 $regsubex($3-,^:(.+),$+(:[style $ifmatch,]\1[/style]))
      }
      else sockwrite -n BV2 %r 
    }
    else { 
      sockwrite -n BV2 WHISPER $comchan($2,1) $2-
    }  
  }
  elseif ($1 == JOIN) && ($numtok($2,44) > 1) {
    var %x 1
    while ($gettok($2,%x,44)) { join $ifmatch | inc %x }
  }
  elseif ($sock(BV2)) sockwrite -n BV2 %r
}

on 1:sockclose:BV2:sockclose BV2.local | BV2 | .timerBV2.recon 0 300 bv2 
on 1:sockclose:BV2.local:sockclose BV2

on 1:open:?:{ var %x 1, %y 
  while ($comchan($nick,%x)) { %y = $addtok(%y,$ifmatch,44) | inc %x }
  echo $nick * Common $+([,$comchan($nick,0),]:) %y | linesep $nick
}

;BV2 - Bot

alias BV2.bot { 
  var %c $$1 , %n | tokenize 32 $iif(%BV2.botinfo. [ $+ [ $2 ] ],$v1,$var(%BV2.botinfo*,1).value)
  var %x BOT[ $+ $1 $+ ]
  if ($sock(%x)) { 
    if ($nick(%c,$1)) %n = 1
    while %c {
      if (!$sock($+(%x,%n))) break
      elseif ($regex($sock($+(%x,%n)).mark,%c)) { if (!%n) %n = 1 | else inc %n }
      elseif (!$regex($sock($+(%x,%n)).mark,%c)) { sockwrite -n $+(%x,%n) JOIN %c | sockmark $+(%x,%n) $sock($+(%x,%n)).mark %c | return }
    }
  }
  sockopen $+(%x,%n) irc.buzzen.com 6667
  sockmark $+(%x,%n) %c
}

alias BV2.botlogin {
  var %n $$input(Bot's FlashIRC Email?,e,Buzzen v2 BETA Login) 
  var %p $md5($$input(Bot's FlashIRC Password?,ep,Buzzen v2 BETA Login))
  set %BV2.botinfo. [ $+ [ %n ] ] %n %p
}

alias BV2.botnick {
  var %n , %x $$1 , %y 1 , %i
  if (%x == 1) { %n = $$2
    while ($sock(BOT[*,%y)) { %i = $ifmatch
      sockwrite -n %i NICK $+(%n,$r(1000000,9999999))
      inc %y
    }
  }
  elseif (%x == 2) {
    while ($sock(BOT[*,%y)) { %i = $ifmatch
      sockwrite -n %i NICK $r(100000000000000,999999999999999)
      inc %y
    }
  }
  else echo $color(kick) -at * BV2.botnick Invalid Parameters.
}

on $1:sockopen:/BOT\[(.+)\]/:{ tokenize 32 %BV2.botinfo. [ $+ [ $regml(1) ] ]
  sockwrite -n $sockname AUTHTYPE ircwx1
  sockwrite -n $sockname LOGINH $1 $2
  sockwrite -n $sockname USER $1 * * $replace(:IRC YSZG I89,I,B,R,V,C,2,Y,v,S,0.,Z,3,8,o,9,t,G,1)
}

on $1:sockread:/BOT\[(.+)\]/:{
  var %c $sock($sockname).mark , %n $regml(1) , %r | sockread %r | tokenize 32 %r
  if ($1 == PING) sockwrite -n $sockname PONG $2-
  if ($2-3 == NOTICE AUTH) && ($regex($4-,bad password) || $regex($4-,user not found)) {
    sockclose $sockname | echo $color(kick) -at * Bot: $+([,%n,]) User Name and/or Password is incorrect.
  }
  elseif ($2 == 433) { 
    sockwrite -n $sockname NICK $3 $+ $sock($+(BOT[,%n,]*),0)
    sockwrite -n $sockname JOIN %c
  }
  elseif ($2 == 001) sockwrite -n $sockname JOIN %c
}

;BV2 - Popups

alias BV2.menu1 {
  if ($1 isin begin end) return
  elseif ($var(%BV2.botinfo.*,$1).value) { return $gettok($ifmatch,1,32) $+ :unset $+($chr(37),BV2.botinfo.,$gettok($ifmatch,1,32)) $chr(124) echo $color(info) -at * Removed Bot: $gettok($ifmatch,1,32) }
}

alias BV2.menu2 {
  if ($1 isin begin end) return
  elseif ($var(%BV2.botinfo.*,$1).value) { return $gettok($ifmatch,1,32) $+ :BV2.bot $chr(35) $gettok($ifmatch,1,32) }
}

menu status,channel,menubar {
  -
  Buzzen V2
  .$iif($sock(BV2),Reconnect,Connect):BV2
  .$iif($sock(BV2),Disconnect):sockclose BV2* | disconnect
  .-
  .$iif(%BV2.info,Reset,Set) Login Info:BV2.login
  .-
  .Access
  ..Echo List:access # list
  ..-
  ..$iif($script(BV2 Extras.mrc),Access Window):BV2.access #
  .$iif($script(BV2 Extras.mrc),Channel List):BV2.list GN
  .$iif($script(BV2 Extras.mrc),Text Emulator)
  ..$iif(%BV2.text,Disable):unset %BV2.text*
  ..-
  ..Settings:BV2.textdiag
  .-
  .Socket Bot
  ..$iif($var(%BV2.botinfo.*),Add/Reset,Set) Login:BV2.botlogin
  ..$iif($var(%BV2.botinfo.*),Delete)
  ...$submenu($BV2.menu1($1))
  ..-
  ..$iif($var(%BV2.botinfo.*),Join Active)
  ...$submenu($BV2.menu2($1))
  ..$iif($sock(BOT[*,0),Part Active):sockwrite -n BOT[* PART #
  ..$iif($sock(BOT[*,0),Kill All Bots):sockclose BOT[*
  ..-
  ..$iif($sock(BOT[*,0),Nick):
  ...Specified Nick:BV2.botnick 1 $$?"Nickname?"
  ...Random Numbers:BV2.botnick 2
  ..$iif($sock(BOT[*,0),Say):sockwrite -n BOT[* PRIVMSG # : $+ $$?"Say?"
}

;BV2 - Stuff

on 1:load:{
  if ($version < 6.2) {
    echo $color(kick) -st * ERROR: Your version of mIRC is too old. Please upgrade to use the BV2 v0.3 Connection. (Requires mIRC v6.2 or greater.)
    unload -nrs $shortfn($script)
  }
}

on 1:unload:{
  if (!$input(Would you like to keep your settings for BV2?,dwy,BV2 Settings)) unset %BV2*
  sockclose BV2* | sockclose BOT[*
}

;EOF

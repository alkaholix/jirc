alias sbot {
  if ($1 = $null) { Echo -a please enter server to connect to koach/b | halt }
  if ($1 = koach) {
    sockclose alkaholik
    window -k0 $+(@a[sockbot]p) 
    sockopen alkaholik irc.koach.com 6667
    set %bot.nick $read(botnick.txt)
  }
  if ($1 = b) {
    sockclose alkaholik
    window -k0 $+(@a[sockbot]p) 
    sockopen alkaholik 192.168.1.154 6667
    set %bot.nick $+(badcore[,$r(1111,9999),])
  }
  if ($1 = ircx) {
    sockclose alkaholik
    window -k0 $+(@a[sockbot]p) 
    sockopen alkaholik irc.ircx.chat 6667
    set %bot.nick $read(botnick.txt)
  }
}
on *:sockopen:alkaholik: {
  sockwrite -n $sockname NICK %bot.nick
  sockwrite -n $sockname USER alkaholik anonymous phenomena : $+(anonymous[sockbot]phenomena By,:Snue)
  sockrename $sockname $+(alkaholik., %bot.nick)
}

on *:sockread:alkaholik.*: {
  var %t, %trig, %s
  %s = $sockname
  %trig = .
  sockread %t 
  tokenize 32 %t
  if ($window(@a[sockbot]p)) {
    echo @a[sockbot]p $1- 
  }
  if ($2 == NOTICE) {
    if (:NickServ!Services@*.com iswm $1) && $regex($4-,registered and protected) {
      sockwrite -n %s privmsg NickServ :identify %bot.pass $lf mode %bot.name -G+B
      if (%ajbot != on) { halt }
      sockwrite -n %s join $replace(%ajbot. [ $+ [ $network ] ],$chr(32),$chr(44)) 
      halt
    }
    if (:NickServ!Services@*.com iswm $1) && $regex($4-,Password incorrect.) {
      sockwrite -n %s nick %bot.nick
      if (%ajbot != on) { halt }
      sockwrite -n %s join $replace(%ajbot. [ $+ [ $network ] ],$chr(32),$chr(44)) 
    }
  }
  if ($2 == PONG) { 
    sockwrite -n %s pong $3
  }
  if ($1 == PING) {
    sockwrite -n %s PONG $2-
  }
  if ($2 == PRIVMSG) && $right($gettok($1,1,33),-1) == $me {
    if $regex($4-,!hop) { 
      set %b.chan $3
      sockwrite -n %s PART $3 $lf JOIN %b.chan
    }
    if $regex($4-,!login) { 
      sockwrite -n %s PRIVMSG Idlerbot login anonymous tequilax
    }
    if $regex($4-,!addaj) { 
      set %ajbot. [ $+ [ $network ] ] $addtok(%ajbot. [ $+ [ $network ] ],$3,32)
      sockwrite -n %s PRIVMSG $3 : $+ $3 Has been added to autojoin
    }
    if $regex($4-,!unhaltaj) { 
      set %ajbot. [ $+ [ $network ] ] $addtok(%ajbot. [ $+ [ $network ] ],$3,32)
      sockwrite -n %s PRIVMSG $3 : $+ $3 Has been Unhalted
    }
    if $regex($4-,!delaj) { 
      set %ajbot. [ $+ [ $network ] ] $remtok(%ajbot. [ $+ [ $network ] ],$3,32)
      sockwrite -n %s PRIVMSG $3 : $+ $3 Has been deleted from autojoin
    }
    if $regex($4-,!haltaj) { 
      set %ajbot. [ $+ [ $network ] ] $remtok(%ajbot. [ $+ [ $network ] ],$3,32)
      sockwrite -n %s PRIVMSG $3 : $+ $3 Has been halted
    }
    if $regex($4-,!massj) { 
      sockwrite -n %s PRIVMSG $3 : $+ Massjoining $5-
      sockwrite -n %s join $replace(%ajbot. [ $+ [ $network ] ],$chr(32),$chr(44)) 
    }
  }
  if ($2 == PRIVMSG)  {
    if $3 == #irpg { halt }
    if $regex($4,!say) { sockwrite -n %s PRIVMSG $3 : $+ $5-
    }
    if $3 == #irpg { halt }
    if $regex($4,!me) { sockwrite -n %s PRIVMSG $3 : $+ ACTION $5-
    }
    if $regex($4-,!time) { 
      if $3 == #irpg { halt }
      sockwrite -n %s PRIVMSG $3 : $+ $left($time(h:nntt) $time(dddd d mmmm yyyy) $+ , $time(yyyy)) 
    }
    if $regex($4-,!google) { 
      if (!$5) { sockwrite -n %s PRIVMSG $3 :http://www.google.com }
      else {
        set %gchan $3
        set %google $replace($5-,$chr(32),+)
        sockopen google google.com 80
      }
    }
  }
  if ($2 == MODE) && $3 == #anonymous {
    if $regex($4-,-Q) { 
      sockwrite -n %s MODE $3 +Q
    }
  }
}
menu channel { 
  Sockbot
  .Join { if ($sock(alkaholik.*)) sockwrite -n alkaholik.* JOIN # }
  .Part { if ($sock(alkaholik.*)) sockwrite -n alkaholik.* PART # }
  .Say { if ($sock(alkaholik.*)) sockwrite -n alkaholik.* PRIVMSG # $$input(Say Something ie: Hi there,e,Say Something) }
  .Custom { if ($sock(alkaholik.*)) sockwrite -n alkaholik.* $$input(Custom Raw ie: mode # +o Snue,e,Custom Modes)  }
  .Create { if ($sock(alkaholik.*)) sockwrite -n alkaholik.* CREATE CP %#badcore %pewpew mptw 50 EN-US 1 badcore 0 }
  .Nick/Password
  ..$iif($var(%bot.name*),Add/Reset,Set) Login:botlogin
  .Autojoin ( $+ $iif(%ajbot,on,off) $+ )
  ..on:set %ajbot on 
  ..off:unset %ajbot
}

alias botlogin {
  var %n $$input(Socketboys Nickname,e,Nickname) 
  set %bot.name %n
  var %p $$input(Socketbots password,ep,Password)
  set %bot.pass %p
}


;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;google;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;

on *:sockopen:define:{
  sockwrite -n $sockname GET /search?hl=en&q=define%3A+ $+ %define $+ &btnI=I%27m+Feeling+Lucky HTTP/1.0
  sockwrite -n $sockname Host: www.google.com
  sockwrite -n $sockname Connection: close
  sockwrite -n $sockname $crlf
}

on *:sockread:define:{
  window -edak0 @i
  sockread -f %itemp
  if ($len(%itemp) > 935) { .echo @i $left(%itemp,935) | .echo @i $right(%itemp,$calc($len(%itemp) - 935)) }
  else { .echo @i %itemp }
  if (definitions of isin %itemp) { sockwrite -n %s PRIVMSG %dchan :Search Result : $remove($gettok(%itemp,16,62),<li) }
}

on *:sockopen:google:{
  sockwrite -n $sockname GET /search?hl=en&q= $+ %google $+ &btnI=I'm+Feeling+Lucky HTTP/1.0
  sockwrite -n $sockname Host: www.google.com
  sockwrite -n $sockname Connection: close
  sockwrite -n $sockname $crlf
}

on *:sockread:google:{
  sockread -f %gtemp
  if (<A HREF=" isin %gtemp) { set %result $gettok($gettok(%gtemp,2,34),1,34) }
}

on *:sockclose:google:{
  sockwrite -n alkaholik.* PRIVMSG %gchan : %result
  unset %gchan
  unset %google
  unset %gtemp
  unset %result
}

;Name: BV2 v0.31 Extras
;Author: napalm

;BV2 - Channels Lister

alias BV2.list { 
  var %c $$1 , %p $2 | unset %BV2.list* 
  set %BV2.list.total 0 | set %BV2.list.ticks $ticks | set %BV2.list.users 0

  if ($sock(BV2.list)) sockclose $ifmatch
  if (!$window(@BV2.list)) window -lkf -t25,29 @BV2.list verdana 10
  BV2.list.buf 1 | titlebar @BV2.list - $BV2.list.flip(%c) $iif(%p,- Page: %p)

  sockopen BV2.list www.flashirc.info 80
  sockmark BV2.list %c %p
}

alias BV2.list.flip return $gettok($BV2.list.cat(2),$findtok($BV2.list.cat(1),$$1,44),44)

alias BV2.list.cat {
  if ($$1 == 1) return UN,GN,RM,CP,PR,IN,NE,SP,CC,LS,ET,RL,RP
  elseif ($$1 == 2) return Unlisted,General,Romance,Computing,Peers,Interests,News & Events,Sports & Politics,City Chats,Lifestyles,Entertainment,Religion,RolePlay
}

alias BV2.list.menu1 {
  if ($1 isin begin end) return
  if ($gettok($BV2.list.cat(2),$1,44)) { var %i $ifmatch | return $+($iif($regex($window(@BV2.list).title,%i),$style(3)) %i,:,BV2.list $gettok($BV2.list.cat(1),$1,44)) }
}

alias BV2.list.menu2 {
  if ($1 <= $gettok(%BV2.list.num,1,32)) return $1 :BV2.list $gettok(%BV2.list.num,2,32) $1
}

alias BV2.list.buf {
  if ($1 == 1) {
    clear @BV2.list
    aline @BV2.list $chr(160)
    aline @BV2.list Room $chr(9) Users $chr(9) Topic
    aline @BV2.list $chr(160)
  }
  if ($1 == 2) {
    aline @BV2.list $chr(160)
    aline @BV2.list - Synched in: $+(,$calc(($ticks - %BV2.list.ticks) / 1000),s) $chr(160) Channels: $+(,%BV2.list.total,) $chr(160) Users: $+(,%BV2.list.users,)
  }
}

on 1:sockopen:BV2.list:{
  tokenize 32 $sock($sockname).mark
  var %str $+(/roomslist.aspx?,$iif($2,$+(pg=,$2,&)),cat=,$1)
  sockwrite -n $sockname POST %str HTTP/1.4
  sockwrite -n $sockname Host: www.flashirc.info
  sockwrite -n $sockname Content-Length: $len(%str) $+ $str($crlf,2) $+ %str
  sockwrite -n $Sockname $crlf
}

on 1:sockread:BV2.list:{
  var %r | sockread %r 
  if ($regex(%r,Title2".Pages:)) && ($regsubex(%r,/^[^<]*>|<[^>]*>|<[^>]*$/g,)) && ($numtok($gettok($v1,2-,32),32) > 1) {
    tokenize 32 $sock($sockname).mark
    set %BV2.list.num $v1 $1
  }
  if ($regex(%r,chatui.+rmlist)) {
    tokenize 1 $regsubex(%r,/^[^<]*>|<[^>]*>|<[^>]*$/g,$chr(1)) | inc %BV2.list.total

    var %u $iif(!$regex(%r,</a></td><td></td>),$4,$3)
    %BV2.list.users = %BV2.list.users + %u

    aline @BV2.list $+($chr(37),$chr(35),$replace($2,$chr(32),\b)) $chr(9) $chr(160) %u $chr(9) $iif(!$regex(%r,</a></td><td></td>),$3)
  }
  if ($regex(%r,</HTML>)) { sockclose $sockname | BV2.list.buf 2 }
}

on *:close:@BV2.list:unset %BV2.list*

menu @BV2.list {
  dclick:join $gettok($sline(@BV2.list,1),1,9)

  Join Channel:join $$gettok($sline(@BV2.list,1),1,9)
  -
  Category
  .$submenu($BV2.list.menu1($1))
  $iif(%BV2.list.num,Select Page)
  .$submenu($BV2.list.menu2($1))
  -
  Channel Link
  .Echo to Status:echo -st * Link: http://www.flashirc.info/chatui.aspx?rm=%25%23 $+ $replace($right($gettok($sline(@BV2.list,1),1,9),-2),\b,+)
  .Clipboard:clipboard http://www.flashirc.info/chatui.aspx?rm=%25%23 $+ $replace($right($gettok($sline(@BV2.list,1),1,9),-2),\b,+)
  .Run in IE:run iexplore http://www.flashirc.info/chatui.aspx?rm=%25%23 $+ $replace($right($gettok($sline(@BV2.list,1),1,9),-2),\b,+)
}

;BV2 - Access Lister

alias BV2.access {
  var %c $$1 | unset %BV2.access*
  if (!$window(@BV2.access)) window -lkf -t5,15 @BV2.access verdana 10
  BV2.access.buf 1 | titlebar @BV2.access - %c
  access %c list
}

alias BV2.access.buf {
  if ($1 == 1) {
    clear @BV2.access
    aline @BV2.access $chr(160)
    aline @BV2.access Level $chr(9) User $chr(9) Mask
    aline @BV2.access $chr(160)
  }
  elseif ($1 == 2) {
    aline @BV2.access $chr(160)
    aline @BV2.access $str($chr(9),2) END OF LIST
  }
}

alias BV2.access.menu1 {
  if ($1 isin begin end) return
  if ($gettok(OWNER:HOST:VOICE:GRANT,$1,58)) { 
    var %i $ifmatch , %c $right($window(@BV2.access).title,-2) 
    return %i :access $!chr(37) $!+ $right(%c,-1) clear %i $chr(124) BV2.access $!chr(37) $!+ $right(%c,-1)
  }
}

alias BV2.access.menu2 {
  if ($1 isin begin end) return
  if ($chan($1)) { var %i $ifmatch | return $!chr(37) $!+ $right(%i,-1) :BV2.access $!chr(37) $!+ $right(%i,-1)  }
}

menu @BV2.access {
  Refresh:BV2.access $right($window(@BV2.access).title,-2)
  -
  Delete:{
    var %c $right($window(@BV2.access).title,-2)
    access %c delete $gettok($sline(@BV2.access,1),1,9) $gettok($sline(@BV2.access,1),3,9) 
    BV2.access %c
  }
  Clear
  .$submenu($BV2.access.menu1($1))
  -
  Get List For..
  .$submenu($BV2.access.menu2($1))
}

;BV2 - Text Emulator

alias BV2.textdiag dialog -m BV2.text BV2.text

alias BV2.texthex {
  var %r , %x $$1 , %y $$2 , %z #000000:Black,#FFFF00:Yellow,#9ACD32:YellowGreen,#00FF00:Lime,#98FB98:PaleGreen,#008000:Green,#006400:DarkGreen,#556B2F:DarkOliveGreen,#808000:Olive,#AFEEEE:PaleTurquoise,#ADD8E6:LightBlue, $+ $&
    #0000FF:Blue,#00BFFF:DeepSkyBlue,#4169E1:RoyalBlue,#000080:Navy,#483D8B:DarkSlateBlue,#DDA0DD:Plum,#9932CC:DarkOrchid,#800080:Purple,#4B0082:Indigo,#2F4F4F:DarkSlateGray,#800000:Maroon,#8B0000:DarkRed, $+ $&
    #FFA500:Orange,#FF8C00:DarkOrange,#48D1CC:MediumTurquoise,#008080:Teal,#FF0000:Red,#A0522D:Sienna,#F4A460:SandyBrown,#2E8B57:SeaGreen,#696969:DimGray,#808080:Gray,#708090:SlateGray,#C0C0C0:Silver, $+ $&
    #FFC0CB:Pink,#FF00FF:Fuchsia,#FF69B4:Hotpink,#00FFFF:Cyan,#FFFACD:LemonChiffon,#F5DEB3:Wheat,#FFD700:Gold,#DAA520:Goldenrod,#B8860B:Darkgoldenrod,#F0E68C:Khaki,#BDB76B:Darkkhaki,#FFFFFF:White
  if (%x isin 1 2) %r = $gettok($gettok(%z,%y,44),%x,58)
  else %r = $gettok($wildtok(%z,$+(*,%x,*),1,44),%y,58)
  return %r
}

dialog BV2.text {
  size -1 -1 238 166
  title "BV2 Text Emulator"
  button "positioner",1001,0 0 0 0 
  box "Color",1,6 83 136 77
  box "Font",2,6 3 136 77
  combo 3,15 48 118 70,drop 
  edit "Current: Arial",4,14 20 118 20, disable center
  edit "Current: Black",5,14 100 118 20, disable center
  combo 6,15 128 118 70,drop 
  box "Style",7,150 3 82 77
  check "Bold",8,166 24 61 20
  check "Italics",9,166 51 61 20
  box "",10,150 83 82 77
  button "Cancel",11,158 128 65 24, cancel 
  button "Accept",12,158 97 65 24, ok 
}

on *:dialog:BV2.text:init:*:{
  didtok $dname 3 44 Arial,Arial Black,Arial Narrow,Book Antiqua,Bookman Old Style,Century Gothic,Comic Sans MS,Courier,Courier New,Fixed Sys,Frankilin Gothic Medium,Garamond, $+ $&
    Georgia,Impact,Lucida Console,Lucida Handwriting,Lucida Sans Unicode,MS Sans Serif,Palatino Lanotype,Papyrus,System,Tahoma,Times New Roman,Trebucht MS,Verdana 
  var %x 1 , %y | while ($BV2.texthex(2,%x)) { %y = $addtok(%y,$ifmatch,44) | inc %x } 
  didtok $dname 6 44 %y
  if (%BV2.text) { tokenize 1 %BV2.textstr
    if ($1) { did -ra $dname 4 Current: $1 | did -c $dname 3 $didwm($dname,3,$1) }
    if ($2) { did -ra $dname 5 Current: $2 | did -c $dname 6 $didwm($dname,6,$2) }
    if ($3) did -c $dname 8
    if ($4) did -c $dname 9
  }
  else {
    did -c $dname 3 1
    did -c $dname 6 1
  }
}

on *:dialog:BV2.text:sclick:12:{ 
  set %BV2.text $replace($+(ff:,$did(3).seltext,co:,$BV2.texthex($did(6).seltext,1),,$iif($did(8).state,b),$iif($did(9).state,i)),$chr(1),$chr(59))
  set %BV2.textstr $+(,$did(3).seltext,,$did(6).seltext,,$iif($did(8).state,b),$iif($did(9).state,i))
}

;BV2 Extras - Stuff

on 1:load:{
  if ($version < 6.2) || (!$script(BV2 v0.31.mrc)) {
    echo $color(kick) -st * ERROR: Your version of mIRC is too old, or you must load the Connection first. (Requires mIRC v6.2 or greater.)
    unload -rs $shortfn($script)
  }
}

;EOF

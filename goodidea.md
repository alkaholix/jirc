# Good ideas — not in mIRC, for review

Things jIRC has (or that were proposed) which **mIRC does not**. Nothing here is
a parity gap; each is a judgement call about whether jIRC should carry it.

Checked against `mirckb-master/docs/source/` (the mIRC 7.84 KB). "Not in mIRC"
means no page exists for it there.

**The rule this file exists to enforce:** if it isn't in mIRC, it goes here for
review rather than into the code. See the audit method note in
`docs/IMPLEMENTATION-AUDIT.md`.

---

## Already shipped — keep, remove, or leave as-is?

### Channel-status shortcuts — `/op` `/deop` `/voice` `/devoice`
**Not in mIRC.** mIRC documents `/mode $chan +o nick` (`mode.rst:79`) and ships
no `/op`. This is the irssi/HexChat convention. jIRC's input bar has always had
them; 26.8.25 made them work from scripts too, since a scripted `/op bob` was
putting a literal `OP bob` on the wire.

*Recommend keeping* — they are deeply established in other clients and users
type them by reflex. Worth a help entry saying they are a jIRC convenience.

### `/halfop` `/owner` `/admin` (and `/dehalfop` `/deowner` `/deadmin`) — **REMOVED**
**Not in mIRC, and not in jIRC's input bar either — added purely because `/op`
existed.** Taken back out on 2026-08-09; this file is now where they live.

If they are ever wanted, the implementation is trivial: they are the same
`cmd_status_mode` path with mode letters `h`, `q` and `a`, and it already stays
silent where the server does not advertise the mode. Nothing needs designing,
which is precisely why it was tempting to add them uninvited.

### Input-bar shortcuts — `/k` `/b` `/w` `/wi` `/wc`
**Not in mIRC.** jIRC's own abbreviations for kick / ban / whisper / whois /
window-close. Long-standing in the input bar; 26.8.25 made them work from
scripts for consistency.

*Recommend keeping* — zero cost, and removing them would break habits.

### `/urls` — the URL grabber list
**Not in mIRC** (mIRC has a URL catcher in its UI, but no `/urls` command).
jIRC-specific.

*Recommend keeping.*

### Owners list — `/aowner`, `$aowner`, `isaowner`
**Not in mIRC.** mIRC has `/aop`, `/avoice` and `/protect` but no owner list.
Added in 26.8.21 at your explicit request after I flagged that mIRC has no
equivalent. Inert where the server does not advertise `q` as a member prefix.

*Keeping — you asked for it.* Noted here only so the parity docs stay honest
about what is jIRC and what is mIRC.

### IRCv3 work — typing indicators, MONITOR, standard-replies, STS
**None of these are mIRC features.** mIRC has its own notify list rather than
MONITOR, and no typing indicators at all. They are modern-protocol support, and
"cross-platform, modern UI" is already listed in the roadmap as where jIRC goes
beyond mIRC.

*Recommend keeping* — this is jIRC's stated differentiator, not scope creep.
Worth deciding whether the roadmap should say so explicitly.

### Icon menubar (26.8.24)
**UI, no mIRC equivalent** — mIRC has File/View/Favorites/Tools/Commands/Window/
Help text menus. You asked for the icon bar directly.

*Keeping — you asked for it.*

---

## Proposed but not built

### jIRC-to-jIRC typing over servers without `message-tags`
Discussed 2026-08-08. Real tags cannot work — the server must relay them — but
the same effect is reachable via CTCP between two jIRC clients, gated on
`CLIENTINFO` so non-jIRC peers never see it. Practical for queries; on plain-IRC
**channels** it would spam everyone and risk flood limits. On IRCX it could ride
`WHISPER`, which is channel-scoped and private, so it fits there.

*Undecided.* You said "it was just a question".

### CTCP requests arriving as IRCX `WHISPER` are never auto-answered
`connection.rs` treats every framed whisper as a *reply*, deliberately, to avoid
two clients auto-replying to each other forever. The consequence is that jIRC
never answers a CTCP that arrives as a whisper — so two jIRC clients on an IRCX
server ignore each other's VERSION.

Not an mIRC parity item (mIRC has no IRCX), but a genuine defect. The fix is to
track outstanding requests: a framed whisper from someone you just CTCP'd is a
reply, otherwise it is a request. **This one is a bug, not a feature idea** — it
is here only because IRCX is outside mIRC's scope.

### Auto-clearing access entries that would defeat a ban
Asked about 2026-08-07. When banning, also drop matching `+e` exempts, IRCX
`ACCESS` entries, and local auto-op entries that would let the person back in.
**mIRC does not do this.** Would need to be opt-in — silently dropping someone's
ops because you banned an old host would be worse than the disease.

*Undecided.*

/** A space inside an IRCX channel/nick name is encoded on the wire so it doesn't
 *  break the protocol. IRC7 uses a literal "\b" (backslash + b); some servers use
 *  the 0x08 backspace byte. Decode both to a real space for *display only* — the
 *  stored name keeps the encoding so outgoing commands still match the server.
 *
 *  Use this for **names**. It is not safe for arbitrary message text, where a
 *  literal "\b" can be legitimate (e.g. a Windows path "C:\backup"). */
export function ircxDisplay(name: string | undefined): string {
  return (name ?? "").replace(/\x08/g, " ").replace(/\\b/g, " ");
}

/** Body-safe IRCX decode for message text: only the 0x08 byte, never the literal
 *  "\b", so chat like "C:\backup" isn't mangled. */
export function ircxBody(text: string): string {
  return text.replace(/\x08/g, " ");
}

// Pending incoming DCC offers, kept so the user can accept them later (via
// `/dcc get <nick>` or the request dialog).
type Offer = { nick: string; ip: string; port: number };
type FileOffer = Offer & { filename: string; size: number };

const chats = new Map<string, Offer>();
const files = new Map<string, FileOffer>();
const key = (serverId: string, nick: string) => `${serverId} ${nick.toLowerCase()}`;

export const dccOffers = {
  set(serverId: string, offer: Offer) {
    chats.set(key(serverId, offer.nick), offer);
  },
  /** Returns and consumes the latest CHAT offer from `nick`. */
  take(serverId: string, nick: string): Offer | undefined {
    const k = key(serverId, nick);
    const o = chats.get(k);
    if (o) chats.delete(k);
    return o;
  },
  setFile(serverId: string, offer: FileOffer) {
    files.set(key(serverId, offer.nick), offer);
  },
  /** Returns and consumes the latest SEND (file) offer from `nick`. */
  takeFile(serverId: string, nick: string): FileOffer | undefined {
    const k = key(serverId, nick);
    const o = files.get(k);
    if (o) files.delete(k);
    return o;
  },
};

// The latest host the server reported for us (USERHOST), used to auto-detect the
// public IP for DCC offers.
let detectedHost = "";
export const dccDetect = {
  set: (host: string) => {
    detectedHost = host;
  },
  get: () => detectedHost,
};

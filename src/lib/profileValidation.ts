import type { ServerProfile } from "./api";

const present = (value?: string) => !!value?.trim();

/**
 * Script-created loopback `/server` connections normally terminate with their
 * one-shot local listener. Retrying the old ephemeral port can never recover;
 * remote scripted servers retain the normal reconnect default.
 */
export function scriptServerAutoReconnect(host: string): boolean {
  const normalized = host.trim().toLowerCase();
  const bare =
    normalized.startsWith("[") && normalized.endsWith("]")
      ? normalized.slice(1, -1)
      : normalized;
  const ipv4 = bare.split(".");
  const ipv4Loopback =
    ipv4.length === 4 &&
    ipv4[0] === "127" &&
    ipv4.every((part) => /^\d{1,3}$/.test(part) && Number(part) <= 255);
  const ipv6Loopback = bare === "::1" || bare === "0:0:0:0:0:0:0:1";
  const localhost = bare === "localhost" || bare === "localhost.";

  return !(ipv4Loopback || ipv6Loopback || localhost);
}

/** Returns a user-facing TLS client-authentication configuration error. */
export function tlsClientAuthError(profile: ServerProfile): string | null {
  const cert = present(profile.tlsClientCertPath);
  const key = present(profile.tlsClientKeyPath);
  const external = !!profile.sasl && profile.saslMechanism === "EXTERNAL";
  const oauthBearer = !!profile.sasl && profile.saslMechanism === "OAUTHBEARER";

  if (external && !profile.tls) return "SASL EXTERNAL requires TLS.";
  if (oauthBearer && !profile.tls) return "SASL OAUTHBEARER requires TLS.";
  if (external && (!cert || !key)) {
    return "SASL EXTERNAL requires both a PEM client certificate and private-key file.";
  }
  if (cert !== key) {
    return "TLS client authentication requires both certificate and private-key files.";
  }
  if ((cert || key) && !profile.tls) {
    return "Enable TLS before using a client certificate.";
  }
  return null;
}

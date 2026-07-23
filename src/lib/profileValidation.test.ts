import { describe, expect, it } from "vitest";
import type { ServerProfile } from "./api";
import { scriptServerAutoReconnect, tlsClientAuthError } from "./profileValidation";

const profile = (extra: Partial<ServerProfile> = {}): ServerProfile => ({
  name: "test",
  host: "irc.example.test",
  port: 6697,
  nick: "me",
  tls: true,
  autojoin: [],
  ...extra,
});

describe("tlsClientAuthError", () => {
  it("accepts ordinary TLS without a client identity", () => {
    expect(tlsClientAuthError(profile())).toBeNull();
  });

  it("requires TLS and both identity files for SASL EXTERNAL", () => {
    expect(
      tlsClientAuthError(profile({ tls: false, sasl: true, saslMechanism: "EXTERNAL" })),
    ).toBe("SASL EXTERNAL requires TLS.");
    expect(
      tlsClientAuthError(profile({ sasl: true, saslMechanism: "EXTERNAL" })),
    ).toContain("both a PEM client certificate");
  });

  it("accepts a complete certificate/key pair and rejects one-sided paths", () => {
    expect(
      tlsClientAuthError(
        profile({
          sasl: true,
          saslMechanism: "EXTERNAL",
          tlsClientCertPath: "client.pem",
          tlsClientKeyPath: "client.key",
        }),
      ),
    ).toBeNull();
    expect(tlsClientAuthError(profile({ tlsClientCertPath: "client.pem" }))).toContain(
      "both certificate and private-key",
    );
  });
});

describe("scriptServerAutoReconnect", () => {
  it("does not retry disposable loopback listeners", () => {
    expect(scriptServerAutoReconnect("127.0.0.1")).toBe(false);
    expect(scriptServerAutoReconnect("127.12.34.56")).toBe(false);
    expect(scriptServerAutoReconnect("LOCALHOST.")).toBe(false);
    expect(scriptServerAutoReconnect("::1")).toBe(false);
    expect(scriptServerAutoReconnect("[::1]")).toBe(false);
  });

  it("preserves reconnects for scripted remote servers", () => {
    expect(scriptServerAutoReconnect("irc.example.test")).toBe(true);
    expect(scriptServerAutoReconnect("192.0.2.1")).toBe(true);
  });
});

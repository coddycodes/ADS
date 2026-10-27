import * as Schema from "effect/Schema";
import { describe, expect, it } from "vite-plus/test";

import {
  RemoteBackendUrlInvalidError,
  RemoteBackendUrlMissingError,
  RemotePairingCodeMissingError,
  RemotePairingTokenMissingError,
  RemotePairingUrlInvalidError,
  getPairingTokenFromUrl,
  readHostedPairingRequest,
  resolveRemotePairingTarget,
  setPairingTokenOnUrl,
  stripPairingTokenFromUrl,
} from "./remote.ts";

const isRemoteBackendUrlInvalidError = Schema.is(RemoteBackendUrlInvalidError);

function resolveDirectHostError(host: string): RemoteBackendUrlInvalidError {
  let error: unknown;
  try {
    resolveRemotePairingTarget({ host, pairingCode: "code" });
  } catch (cause) {
    error = cause;
  }
  expect(error, host).toBeInstanceOf(RemoteBackendUrlInvalidError);
  if (!isRemoteBackendUrlInvalidError(error)) {
    throw new Error(`Expected RemoteBackendUrlInvalidError for ${host}`);
  }
  expect(error.source, host).toBe("direct-host");
  return error;
}

describe("remote", () => {
  it("derives backend urls and token from a pairing url", () => {
    expect(
      resolveRemotePairingTarget({
        pairingUrl: "https://remote.example.com/pair#token=pairing-token",
      }),
    ).toEqual({
      credential: "pairing-token",
      httpBaseUrl: "https://remote.example.com/",
      wsBaseUrl: "wss://remote.example.com/",
    });
  });

  it("accepts pairing urls that still use a query token", () => {
    expect(
      resolveRemotePairingTarget({
        pairingUrl: "https://remote.example.com/pair?token=pairing-token",
      }),
    ).toEqual({
      credential: "pairing-token",
      httpBaseUrl: "https://remote.example.com/",
      wsBaseUrl: "wss://remote.example.com/",
    });
  });

  it("derives backend urls from hosted app pairing links", () => {
    expect(
      resolveRemotePairingTarget({
        pairingUrl:
          "https://app.coddy.id/pair?host=https%3A%2F%2Fdesktop.tailnet.ts.net%3A44342%2F#token=pairing-token",
      }),
    ).toEqual({
      credential: "pairing-token",
      httpBaseUrl: "https://desktop.tailnet.ts.net:44342/",
      wsBaseUrl: "wss://desktop.tailnet.ts.net:44342/",
    });
  });

  it("derives backend urls from a host and pairing code", () => {
    expect(
      resolveRemotePairingTarget({
        host: "https://remote.example.com",
        pairingCode: "pairing-token",
      }),
    ).toEqual({
      credential: "pairing-token",
      httpBaseUrl: "https://remote.example.com/",
      wsBaseUrl: "wss://remote.example.com/",
    });
  });

  it("preserves host ports when normalizing a bare host input", () => {
    expect(
      resolveRemotePairingTarget({
        host: "myserver.com:3000",
        pairingCode: "pairing-token",
      }),
    ).toEqual({
      credential: "pairing-token",
      httpBaseUrl: "https://myserver.com:3000/",
      wsBaseUrl: "wss://myserver.com:3000/",
    });
  });

  it("accepts single-letter bare hosts with ports for direct and hosted inputs", () => {
    expect(
      resolveRemotePairingTarget({
        host: "x:8080",
        pairingCode: "direct-code",
      }),
    ).toEqual({
      credential: "direct-code",
      httpBaseUrl: "https://x:8080/",
      wsBaseUrl: "wss://x:8080/",
    });
    expect(
      resolveRemotePairingTarget({
        pairingUrl: "https://app.coddy.id/pair?host=C%3A4100%2Fhealth#token=hosted-code",
      }),
    ).toEqual({
      credential: "hosted-code",
      httpBaseUrl: "https://c:4100/",
      wsBaseUrl: "wss://c:4100/",
    });
  });

  it("normalizes protocol-relative hosts as HTTPS", () => {
    const cases = [
      {
        host: "//remote.example.com:8443/path",
        httpBaseUrl: "https://remote.example.com:8443/",
        wsBaseUrl: "wss://remote.example.com:8443/",
      },
      {
        host: "//devbox/path",
        httpBaseUrl: "https://devbox/",
        wsBaseUrl: "wss://devbox/",
      },
      {
        host: "//server/share/coddy-studio",
        httpBaseUrl: "https://server/",
        wsBaseUrl: "wss://server/",
      },
      {
        host: "//localhost/path",
        httpBaseUrl: "https://localhost/",
        wsBaseUrl: "wss://localhost/",
      },
      {
        host: "//127.0.0.1:4100/path",
        httpBaseUrl: "https://127.0.0.1:4100/",
        wsBaseUrl: "wss://127.0.0.1:4100/",
      },
      {
        host: "//[::1]:4200/path",
        httpBaseUrl: "https://[::1]:4200/",
        wsBaseUrl: "wss://[::1]:4200/",
      },
    ];

    for (const { host, httpBaseUrl, wsBaseUrl } of cases) {
      expect(resolveRemotePairingTarget({ host, pairingCode: " pairing-token " }), host).toEqual({
        credential: "pairing-token",
        httpBaseUrl,
        wsBaseUrl,
      });
    }
  });

  it("rejects every raw backslash before URL parsing", () => {
    const rawBackslashInputs = [
      "//\\\\server\\\\share",
      "https://\\\\server\\\\share",
      "ws://\\\\server\\\\socket",
      "https://user:\\secret@remote.example.com",
      "https://remote.example.com/path\\segment",
      "https://remote.example.com/?path=one\\two",
      "remote.example.com\\path",
    ];

    for (const host of rawBackslashInputs) {
      const error = resolveDirectHostError(host);
      expect(error.cause, host).toBeUndefined();
      expect(error.protocol, host).toBeUndefined();
    }
  });

  it("preserves representative bare authorities, paths, query, and fragment", () => {
    const bareAuthorities = [
      {
        host: "devbox:4100/path?query=yes#fragment",
        httpBaseUrl: "https://devbox:4100/",
        wsBaseUrl: "wss://devbox:4100/",
      },
      {
        host: "localhost/path",
        httpBaseUrl: "https://localhost/",
        wsBaseUrl: "wss://localhost/",
      },
      {
        host: "127.0.0.1:4200/path",
        httpBaseUrl: "https://127.0.0.1:4200/",
        wsBaseUrl: "wss://127.0.0.1:4200/",
      },
      {
        host: "[::1]:4300/path",
        httpBaseUrl: "https://[::1]:4300/",
        wsBaseUrl: "wss://[::1]:4300/",
      },
      {
        host: "[2001:db8::1]/path?query=yes#fragment",
        httpBaseUrl: "https://[2001:db8::1]/",
        wsBaseUrl: "wss://[2001:db8::1]/",
      },
      {
        host: "http:80/path",
        httpBaseUrl: "https://http:80/",
        wsBaseUrl: "wss://http:80/",
      },
      {
        host: "ftp:21/path",
        httpBaseUrl: "https://ftp:21/",
        wsBaseUrl: "wss://ftp:21/",
      },
    ];

    for (const { host, httpBaseUrl, wsBaseUrl } of bareAuthorities) {
      expect(resolveRemotePairingTarget({ host, pairingCode: "code" }), host).toEqual({
        credential: "code",
        httpBaseUrl,
        wsBaseUrl,
      });
    }
  });

  it("rejects backend URLs containing userinfo", () => {
    for (const host of [
      "https://trusted.example@attacker.example:8080",
      "https://user:password@example.test",
    ]) {
      const error = resolveDirectHostError(host);
      expect(error.cause, host).toBeUndefined();
      expect(error.protocol, host).toBeUndefined();
    }
  });

  it("validates bare hostname port ranges through URL parsing", () => {
    for (const host of ["devbox:65536/path", "http:65536/path"]) {
      const error = resolveDirectHostError(host);
      expect(error.cause, host).toBeInstanceOf(TypeError);
      expect(error.protocol, host).toBeUndefined();
    }
  });

  it("allows colons after the leading bare authority token", () => {
    const cases = [
      {
        host: "devbox/path:segment",
        httpBaseUrl: "https://devbox/",
        wsBaseUrl: "wss://devbox/",
      },
      {
        host: "devbox?time=12:30",
        httpBaseUrl: "https://devbox/",
        wsBaseUrl: "wss://devbox/",
      },
      {
        host: "devbox#section:one",
        httpBaseUrl: "https://devbox/",
        wsBaseUrl: "wss://devbox/",
      },
      {
        host: "devbox?redirect=https://nested.example/x",
        httpBaseUrl: "https://devbox/",
        wsBaseUrl: "wss://devbox/",
      },
      {
        host: "[2001:db8::1]:4400/path:segment?time=12:30#section:one",
        httpBaseUrl: "https://[2001:db8::1]:4400/",
        wsBaseUrl: "wss://[2001:db8::1]:4400/",
      },
      {
        host: "[2001:db8::2]?redirect=https://[2001:db8::3]/x:y",
        httpBaseUrl: "https://[2001:db8::2]/",
        wsBaseUrl: "wss://[2001:db8::2]/",
      },
    ];

    for (const { host, httpBaseUrl, wsBaseUrl } of cases) {
      expect(resolveRemotePairingTarget({ host, pairingCode: "code" }), host).toEqual({
        credential: "code",
        httpBaseUrl,
        wsBaseUrl,
      });
    }
  });

  it("rejects Windows and POSIX local path forms as direct backend hosts", () => {
    const localPaths = [
      "C:\\Users\\dev\\coddy-studio",
      "C:/Users/dev/coddy-studio",
      "C:folder",
      "C:relative\\coddy-studio",
      "\\\\server\\share\\coddy-studio",
      "\\\\?\\C:\\Users\\dev\\coddy-studio",
      "\\\\.\\pipe\\coddy-studio",
      "\\Users\\dev\\coddy-studio",
      "/var/run/coddy-studio.sock",
      "./relative/coddy-studio",
      "../relative/coddy-studio",
      ".\\relative\\coddy-studio",
      "..\\relative\\coddy-studio",
    ];

    for (const host of localPaths) {
      const error = resolveDirectHostError(host);
      expect(error.cause, host).toBeUndefined();
      expect(error.protocol, host).toBeUndefined();
    }
  });

  it("rejects unsupported explicit RFC-style schemes without rewriting them", () => {
    const unsupportedSchemes = [
      { host: "a.b://remote.example.com/path", protocol: "a.b:" },
      { host: "git+ssh://remote.example.com/path", protocol: "git+ssh:" },
      { host: "ftp://remote.example.com/path", protocol: "ftp:" },
    ];

    for (const { host, protocol } of unsupportedSchemes) {
      const error = resolveDirectHostError(host);
      expect(error.protocol, host).toBe(protocol);
      expect(error.cause, host).toBeUndefined();
    }
  });

  it("rejects invalid explicit scheme syntax before URL parsing", () => {
    for (const host of ["1http://remote.example.com", "a_b://remote.example.com", "://devbox"]) {
      const error = resolveDirectHostError(host);
      expect(error.cause, host).toBeUndefined();
      expect(error.protocol, host).toBeUndefined();
    }
  });

  it("rejects remaining colon forms instead of defaulting them to HTTPS", () => {
    const colonForms = [
      { host: "mailto:user@example.com", protocol: "mailto:" },
      { host: "a.b:value", protocol: "a.b:" },
      { host: "custom:value", protocol: "custom:" },
      { host: "not_a_scheme:value" },
      { host: "ftp:/remote.example.com", protocol: "ftp:" },
      { host: "http:/remote.example.com" },
      { host: "https:/remote.example.com" },
      { host: "ws:/remote.example.com" },
      { host: "wss:/remote.example.com" },
    ];

    for (const { host, protocol } of colonForms) {
      const error = resolveDirectHostError(host);
      expect(error.cause, host).toBeUndefined();
      expect(error.protocol, host).toBe(protocol);
    }
  });

  it("rejects malformed supported schemes and missing or invalid authorities", () => {
    const malformedHosts = [
      "http:remote.example.com",
      "wss:remote.example.com",
      "//",
      "///devbox",
      "//:4100/path",
      "https://",
      "https:///devbox",
      "http://:4100",
      "ws://[invalid",
    ];

    for (const host of malformedHosts) {
      const error = resolveDirectHostError(host);
      expect(error.protocol, host).toBeUndefined();
    }
  });

  it("rejects local paths carried by hosted pairing requests with their source", () => {
    let error: unknown;
    try {
      resolveRemotePairingTarget({
        pairingUrl: "https://app.coddy.id/pair?host=C%3A%5CUsers%5Cdev%5Cbibcode#token=code",
      });
    } catch (cause) {
      error = cause;
    }

    expect(error).toBeInstanceOf(RemoteBackendUrlInvalidError);
    expect(error).toMatchObject({ source: "hosted-pairing-host" });
    expect((error as RemoteBackendUrlInvalidError).cause).toBeUndefined();
    expect((error as RemoteBackendUrlInvalidError).protocol).toBeUndefined();
  });

  it("normalizes ws and wss backend schemes", () => {
    expect(
      resolveRemotePairingTarget({ host: "ws://127.0.0.1:4100/path", pairingCode: "code" }),
    ).toEqual({
      credential: "code",
      httpBaseUrl: "http://127.0.0.1:4100/",
      wsBaseUrl: "ws://127.0.0.1:4100/",
    });
    expect(
      resolveRemotePairingTarget({
        pairingUrl: "wss://[::1]:4200/pair?ignored=yes#token=code",
      }),
    ).toEqual({
      credential: "code",
      httpBaseUrl: "https://[::1]:4200/",
      wsBaseUrl: "wss://[::1]:4200/",
    });
    expect(
      resolveRemotePairingTarget({
        pairingUrl: "http://localhost:4300/pair#token=code",
      }),
    ).toEqual({
      credential: "code",
      httpBaseUrl: "http://localhost:4300/",
      wsBaseUrl: "ws://localhost:4300/",
    });
  });

  it("rejects unsupported direct pairing URL protocols", () => {
    let pairingUrlError: unknown;
    try {
      resolveRemotePairingTarget({
        pairingUrl: "ftp://remote.example.com/pair#token=pairing-token",
      });
    } catch (cause) {
      pairingUrlError = cause;
    }

    expect(pairingUrlError).toBeInstanceOf(RemotePairingUrlInvalidError);
    expect(pairingUrlError).toMatchObject({ protocol: "ftp:" });
    expect((pairingUrlError as RemotePairingUrlInvalidError).cause).toBeUndefined();
  });

  it("rejects unsupported hosted pairing backend protocols", () => {
    let hostError: unknown;
    try {
      resolveRemotePairingTarget({
        pairingUrl:
          "https://app.coddy.id/pair?host=ftp%3A%2F%2Fremote.example.com#token=pairing-token",
      });
    } catch (cause) {
      hostError = cause;
    }

    expect(hostError).toBeInstanceOf(RemoteBackendUrlInvalidError);
    expect(hostError).toMatchObject({ source: "hosted-pairing-host", protocol: "ftp:" });
    expect((hostError as RemoteBackendUrlInvalidError).cause).toBeUndefined();
  });

  it("rejects unsupported direct host protocols", () => {
    let hostError: unknown;
    try {
      resolveRemotePairingTarget({
        host: "ftp://remote.example.com",
        pairingCode: "pairing-token",
      });
    } catch (cause) {
      hostError = cause;
    }

    expect(hostError).toBeInstanceOf(RemoteBackendUrlInvalidError);
    expect(hostError).toMatchObject({ source: "direct-host", protocol: "ftp:" });
    expect((hostError as RemoteBackendUrlInvalidError).cause).toBeUndefined();
  });

  it("uses distinct structural errors for missing pairing inputs", () => {
    expect(() => resolveRemotePairingTarget({})).toThrowError(RemoteBackendUrlMissingError);
    expect(() =>
      resolveRemotePairingTarget({ pairingUrl: "https://remote.example.com/pair" }),
    ).toThrowError(RemotePairingTokenMissingError);
    expect(() =>
      resolveRemotePairingTarget({
        host: "https://remote.example.com/path?token=sensitive#fragment",
      }),
    ).toThrowError(
      expect.objectContaining({
        _tag: "RemotePairingCodeMissingError",
        host: "remote.example.com",
      }),
    );
  });

  it("preserves URL parsing causes with their input source", () => {
    let pairingUrlError: unknown;
    try {
      resolveRemotePairingTarget({ pairingUrl: "not a url" });
    } catch (cause) {
      pairingUrlError = cause;
    }
    expect(pairingUrlError).toBeInstanceOf(RemotePairingUrlInvalidError);
    expect((pairingUrlError as RemotePairingUrlInvalidError).cause).toBeInstanceOf(TypeError);

    let hostError: unknown;
    try {
      resolveRemotePairingTarget({ host: "https://[invalid", pairingCode: "pairing-token" });
    } catch (cause) {
      hostError = cause;
    }
    expect(hostError).toBeInstanceOf(RemoteBackendUrlInvalidError);
    expect(hostError).toMatchObject({ source: "direct-host" });
    expect((hostError as RemoteBackendUrlInvalidError).cause).toBeInstanceOf(TypeError);
  });

  it("exposes stable messages for every pairing error", () => {
    expect(new RemoteBackendUrlMissingError().message).toBe("Enter a backend URL.");
    expect(new RemotePairingUrlInvalidError({}).message).toBe("Pairing URL is invalid.");
    expect(new RemoteBackendUrlInvalidError({ source: "direct-host" }).message).toBe(
      "Backend URL is invalid.",
    );
    expect(new RemotePairingTokenMissingError({ host: "example.com" }).message).toBe(
      "Pairing URL is missing its token.",
    );
    expect(new RemotePairingCodeMissingError({ host: "example.com" }).message).toBe(
      "Enter a pairing code.",
    );
  });

  it("reads hash tokens before query tokens and trims both forms", () => {
    expect(
      getPairingTokenFromUrl(
        new URL("https://remote.example.com/pair?token=query#token=%20hash%20"),
      ),
    ).toBe("hash");
    expect(
      getPairingTokenFromUrl(new URL("https://remote.example.com/pair?token=%20query%20#other=1")),
    ).toBe("query");
    expect(getPairingTokenFromUrl(new URL("https://remote.example.com/pair#token=%20"))).toBeNull();
  });

  it("strips pairing tokens while preserving unrelated query and hash values", () => {
    const stripped = stripPairingTokenFromUrl(
      new URL(
        "https://remote.example.com/pair?token=query&tab=one&host=devbox&action=add-server#token=hash&label=Desk",
      ),
    );
    expect(stripped.toString()).toBe(
      "https://remote.example.com/pair?tab=one&host=devbox&action=add-server#label=Desk",
    );

    const queryOnly = stripPairingTokenFromUrl(
      new URL("https://remote.example.com/pair?token=query#label=Desk"),
    );
    expect(queryOnly.toString()).toBe("https://remote.example.com/pair#label=Desk");
  });

  it("sets an encoded hash token and removes a stale query token", () => {
    const original = new URL("https://remote.example.com/pair?token=old&tab=one#label=old");
    const next = setPairingTokenOnUrl(original, "spaced token / ü");

    expect(original.toString()).toContain("token=old");
    expect(next.searchParams.get("token")).toBeNull();
    expect(next.searchParams.get("tab")).toBe("one");
    expect(getPairingTokenFromUrl(next)).toBe("spaced token / ü");
  });

  it("reads complete hosted requests and rejects incomplete ones", () => {
    expect(
      readHostedPairingRequest(
        new URL(
          "https://app.coddy.id/pair?host=%20https%3A%2F%2Flan.example%3A444%20&label=%20Office%20#token=%20secret%20",
        ),
      ),
    ).toEqual({
      httpBaseUrl: "https://lan.example:444/",
      displayHost: "lan.example:444",
      token: "secret",
      label: "Office",
    });
    expect(
      readHostedPairingRequest(new URL("https://app.coddy.id/pair?host=example.com")),
    ).toBeNull();
    expect(
      readHostedPairingRequest(new URL("https://app.coddy.id/pair#token=secret")),
    ).toBeNull();
  });

  it("rejects hosted backend userinfo and exposes an ASCII display host", () => {
    for (const host of [
      "https://trusted.example@attacker.example:8080",
      "https://user:password@example.test",
    ]) {
      const url = new URL("https://app.coddy.id/pair#token=secret");
      url.searchParams.set("host", host);
      expect(readHostedPairingRequest(url), host).toBeNull();
    }

    const idnUrl = new URL("https://app.coddy.id/pair#token=secret");
    idnUrl.searchParams.set("host", "https://аpple.com");
    expect(readHostedPairingRequest(idnUrl)).toEqual({
      httpBaseUrl: "https://xn--pple-43d.com/",
      displayHost: "xn--pple-43d.com",
      token: "secret",
      label: "",
    });
  });
});

// TypeScript port of acb_policy::url_validator. Fed by `GET /config`, which
// hands us the rule names + scheme deny list. We mirror the Rust validator's
// decision order so the location bar's color matches what the daemon will
// say. The actual server-side decision is still authoritative.

export type RuleSummary = {
  name: string;
  kind: "regex" | "fqdn" | "ip_cidr";
  // The redacted /config doesn't return rule details; this purely-client
  // approximation is for UX, not security. Any wrong "allowed" prediction
  // is corrected when the user submits and the server replies 403.
  allowed_classes?: string[];
};

export type ConfigSummary = {
  etag: string;
  rules: RuleSummary[];
  always_block_schemes: string[];
  subresources_inherit_page: boolean;
  helper_sha256: string;
  // Rendered viewport size the daemon pins Chromium to. The Viewport
  // component scales mouse coordinates against this so clicks land right.
  viewport: { width: number; height: number };
};

export type ValidateResult =
  | { ok: true; rule: string }
  | { ok: false; reason: string };

export function validateUrl(raw: string, cfg: ConfigSummary): ValidateResult {
  let u: URL;
  try {
    u = new URL(raw);
  } catch (e) {
    return { ok: false, reason: "malformed url" };
  }
  const scheme = u.protocol.replace(/:$/, "").toLowerCase();
  if (cfg.always_block_schemes.map((s) => s.toLowerCase()).includes(scheme)) {
    return { ok: false, reason: `scheme ${scheme} denied` };
  }
  if (scheme !== "http" && scheme !== "https") {
    return { ok: false, reason: `scheme ${scheme} denied` };
  }
  if (u.username || u.password) {
    return { ok: false, reason: "userinfo not allowed" };
  }
  // We can't predict regex/cidr matching client-side without the full rule
  // bodies. We return a soft-allow ("rule unknown") so the location bar
  // shows neutral, then the server is authoritative on submit.
  return { ok: true, rule: "(server decides)" };
}

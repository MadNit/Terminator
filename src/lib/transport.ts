import type { TransportSpec } from "./api";

export type Kind = TransportSpec["kind"];

/**
 * RDP is implemented (NLA/CredSSP with a password). Kept as a flag rather than
 * deleted because the connect path still needs somewhere to express "this
 * profile cannot be opened right now".
 */
export const RDP_IMPLEMENTED = true;

/** Sidebar section order. Groups with no profiles are not rendered. */
export const KIND_ORDER: Kind[] = ["ssh", "rdp", "local"];

export const KIND_LABEL: Record<Kind, string> = {
  ssh: "SSH",
  rdp: "RDP",
  local: "Local",
};

export const kindBadge = (k: Kind) => (k === "local" ? "sh" : k);

/** Host:port for remote specs, a friendly label for local shells. */
export function describeTarget(spec: TransportSpec): string {
  if (spec.kind === "local") return spec.shell || "default shell";
  return `${spec.user}@${spec.host}:${spec.port}`;
}

export function describeAuth(spec: TransportSpec): string {
  // RDP is password-only: CredSSP needs the actual secret, so there is no
  // agent or key equivalent to describe.
  if (spec.kind !== "ssh") return "password";
  switch (spec.auth.method) {
    case "agent":
      return "ssh-agent";
    case "key":
      return `key: ${spec.auth.path}`;
    default:
      return "password";
  }
}

/** Whether this profile can be connected right now. */
export function connectBlockedReason(spec: TransportSpec): string | null {
  if (spec.kind === "rdp" && !RDP_IMPLEMENTED) {
    return "RDP is not implemented yet. The profile is saved and will work once the RDP transport lands.";
  }
  return null;
}

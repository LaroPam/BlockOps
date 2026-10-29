/** A coordinator code: Crockford alphabet without I, L, O and U, as XXXX-XXXX-XXXX. */
const CODE = /\b[0-9A-HJKMNP-TV-Z]{4}-[0-9A-HJKMNP-TV-Z]{4}-[0-9A-HJKMNP-TV-Z]{4}\b/g;

export type CodeStatus = { id: string; code: string; expires_at: number; phase: string; peer: string | null; error: string | null };

/** The message a host sends to a friend; the code is its only secret. */
export function invitationText(code: string, expiresAt: number): string {
  const until = new Date(expiresAt * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  return `Заходи в мой мир Minecraft через BlockOps! Код приглашения: ${code}. Он одноразовый и действует до ${until}.`;
}

/** The code inside a pasted invitation, or null unless exactly one is present. */
export function findCode(text: string): string | null {
  const found = text.toUpperCase().match(CODE) ?? [];
  return found.length === 1 ? found[0] : null;
}

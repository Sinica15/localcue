export type LinkStatus = "connecting" | "open" | "closed";

export interface Link {
  /** Returns false when not connected. */
  send(msg: Record<string, unknown>): boolean;
  close(): void;
}

export function connect(opts: {
  url: string | (() => string | Promise<string>);
  onMessage: (msg: any) => void;
  onStatus?: (status: LinkStatus) => void;
}): Link;

// Host-injected globals: real Tauri runtime in the app, shim in the preview.
export type TauriInvoke = (
  cmd: string,
  args?: Record<string, unknown>,
) => Promise<any>;
export type TauriListen = (
  event: string,
  handler: (message: { payload: any }) => void,
) => Promise<() => void>;

declare global {
  interface Window {
    __TAURI__?: { core: { invoke: TauriInvoke }; event: { listen: TauriListen } };
    __HERDR_SHELL__?: string;
    __HERDR_APP__?: unknown;
  }
}

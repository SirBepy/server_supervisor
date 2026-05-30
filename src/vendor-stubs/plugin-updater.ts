export interface Update {
  version: string;
  body?: string;
  downloadAndInstall(onEvent?: (e: DownloadEvent) => void): Promise<void>;
}
export interface DownloadEvent {
  event: string;
  data?: { contentLength?: number; chunkLength?: number };
}
export async function check(): Promise<null> {
  return null;
}

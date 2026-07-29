export async function applicationVersion(): Promise<string> {
  if (import.meta.env.MODE === "fixture" || import.meta.env.MODE === "test") {
    return "0.1.0-fixture";
  }

  const { getVersion } = await import("@tauri-apps/api/app");
  return getVersion();
}

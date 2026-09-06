export interface KnownDevice {
  hostId: string;
  name: string;
  url: string;
  token?: string;
  version?: string;
  lastSeen: string;
}

const STORAGE_KEY = "nexus.known-hosts";

export function knownDevices(): KnownDevice[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "[]");
    if (!Array.isArray(value)) return [];
    return value.filter(isKnownDevice);
  } catch {
    return [];
  }
}

export function rememberDevice(device: KnownDevice): KnownDevice[] {
  const devices = knownDevices().filter((item) => item.hostId !== device.hostId && item.url !== device.url);
  devices.unshift(device);
  localStorage.setItem(STORAGE_KEY, JSON.stringify(devices.slice(0, 12)));
  return devices.slice(0, 12);
}

export function forgetDevice(hostId: string): KnownDevice[] {
  const devices = knownDevices().filter((item) => item.hostId !== hostId);
  localStorage.setItem(STORAGE_KEY, JSON.stringify(devices));
  return devices;
}

function isKnownDevice(value: unknown): value is KnownDevice {
  if (!value || typeof value !== "object") return false;
  const item = value as Partial<KnownDevice>;
  return typeof item.hostId === "string" && typeof item.name === "string" && typeof item.url === "string";
}

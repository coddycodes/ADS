import { classifyPairingEndpoint } from "@coddy/shared/advertisedEndpoint";

export type ShareEndpointClass = "loopback" | "off-host" | "unconnectable";

export function shareClassForPairingEndpoint(endpoint: string): ShareEndpointClass {
  switch (classifyPairingEndpoint(endpoint)) {
    case "loopback":
      return "loopback";
    case "private-network":
    case "public":
      return "off-host";
    case "unconnectable":
      return "unconnectable";
  }
}

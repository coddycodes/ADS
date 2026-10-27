import { useClerk } from "@clerk/react";

export function useCoddyConnectAuthPrompt() {
  const clerk = useClerk();
  const openAuthPrompt = () => {
    clerk.openWaitlist();
  };
  return { authPrompt: null, openAuthPrompt };
}

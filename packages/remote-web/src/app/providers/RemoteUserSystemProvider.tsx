import { ReactNode, useCallback, useMemo } from "react";
import { useParams } from "@tanstack/react-router";
import { configApi } from "@/shared/lib/api";
import { useAuth } from "@/shared/hooks/auth/useAuth";
import { useUserSystemController } from "@/shared/hooks/useUserSystemController";
import { UserSystemContext } from "@/shared/hooks/useUserSystem";
import { ThemeProvider } from "@/shared/providers/ThemeProvider";
import { ThemeMode } from "shared/types";
import type { ConfigWithAppearance } from "@/shared/lib/themeCustomizations";

interface RemoteUserSystemProviderProps {
  children: ReactNode;
}

export function RemoteUserSystemProvider({
  children,
}: RemoteUserSystemProviderProps) {
  const { isSignedIn, isLoaded } = useAuth();
  const { hostId } = useParams({ strict: false });
  const loadConfig = useCallback(() => configApi.getConfig(), []);
  const saveConfig = useCallback(
    (config: Parameters<typeof configApi.saveConfig>[0]) =>
      configApi.saveConfig(config),
    [],
  );
  const userSystemQueryKey = useMemo(
    () => ["user-system", "remote-route", hostId] as const,
    [hostId],
  );
  const { value, isLoading } = useUserSystemController({
    queryKey: userSystemQueryKey,
    enabled: isLoaded && isSignedIn && !!hostId,
    load: loadConfig,
    save: saveConfig,
  });

  const typedConfig = value.config as ConfigWithAppearance | null;

  const contextValue = useMemo(
    () => ({
      ...value,
      loading: !isLoaded || (isSignedIn && isLoading),
    }),
    [isLoaded, isLoading, isSignedIn, value],
  );

  return (
    <UserSystemContext.Provider value={contextValue}>
      <ThemeProvider
        initialTheme={typedConfig?.theme || ThemeMode.SYSTEM}
        appearance={typedConfig?.appearance}
      >
        {children}
      </ThemeProvider>
    </UserSystemContext.Provider>
  );
}

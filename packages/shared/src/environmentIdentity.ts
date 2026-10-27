export function readCoddyStudioEnvironmentVariable(
  env: Readonly<Record<string, string | undefined>>,
  suffix: string,
): string | undefined {
  return env[`CODDY_STUDIO_${suffix}`];
}

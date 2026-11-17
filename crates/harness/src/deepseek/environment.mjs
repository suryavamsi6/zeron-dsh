export function trustedToolEnvironment(source) {
  const result = {};
  if (source.GH_TOKEN !== undefined) result.GH_TOKEN = source.GH_TOKEN;
  if (source.GIT_CONFIG_COUNT !== undefined) {
    const count = Number(source.GIT_CONFIG_COUNT);
    if (!Number.isInteger(count) || count < 0 || count > 64) throw new Error('Invalid trusted Git configuration count');
    result.GIT_CONFIG_COUNT = String(count);
    for (let index = 0; index < count; index++) {
      for (const part of ['KEY', 'VALUE']) {
        const key = `GIT_CONFIG_${part}_${index}`;
        if (typeof source[key] !== 'string') throw new Error('Incomplete trusted Git configuration');
        result[key] = source[key];
      }
    }
  }
  return result;
}

export function runtimeEnvironment(source) {
  const allowed = new Set(['PATH', 'HOME', 'USERPROFILE', 'LOCALAPPDATA', 'APPDATA',
    'SystemRoot', 'SYSTEMROOT', 'WINDIR', 'COMSPEC', 'PATHEXT', 'TMPDIR', 'TMP', 'TEMP',
    'LANG', 'TZ', 'SHELL', 'SSL_CERT_FILE', 'SSL_CERT_DIR', 'NODE_EXTRA_CA_CERTS',
    'HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'NO_PROXY', 'http_proxy', 'https_proxy',
    'all_proxy', 'no_proxy', 'DEEPSEEK_API_KEY', 'DEEPSEEK_BASE_URL']);
  return { ...Object.fromEntries(Object.entries(source).filter(([key, value]) =>
    value !== undefined && (allowed.has(key) || key.startsWith('LC_')))), ...trustedToolEnvironment(source) };
}

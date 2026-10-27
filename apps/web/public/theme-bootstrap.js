(() => {
  const LIGHT_BACKGROUND = "#ffffff";
  const DARK_BACKGROUND = "#161616";
  const themeColorMeta = document.querySelector('meta[name="theme-color"]');
  try {
    const storedTheme = window.localStorage.getItem("coddy-studio:theme");
    const theme =
      storedTheme === "light" || storedTheme === "dark" || storedTheme === "system"
        ? storedTheme
        : "system";
    const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    const isDark = theme === "dark" || (theme === "system" && prefersDark);
    document.documentElement.classList.toggle("dark", isDark);
    const chromeColor = isDark ? DARK_BACKGROUND : LIGHT_BACKGROUND;
    document.documentElement.style.backgroundColor = chromeColor;
    themeColorMeta?.setAttribute("content", chromeColor);
  } catch {
    document.documentElement.classList.add("dark");
    document.documentElement.style.backgroundColor = DARK_BACKGROUND;
    themeColorMeta?.setAttribute("content", DARK_BACKGROUND);
  }
})();

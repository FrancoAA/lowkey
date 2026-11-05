// The window's colours, set before the page paints (a plain script in <head>, so the page never
// flashes the wrong theme). The choice is in config.toml (Settings → General → Appearance); this
// copy in localStorage is only for the first paint. main.ts calls lowkeyTheme() when it changes.
(function () {
  var media = window.matchMedia("(prefers-color-scheme: dark)");
  var choice = "system";
  function paint() {
    var dark = choice === "dark" || (choice === "system" && media.matches);
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    document.dispatchEvent(new Event("lowkey-theme")); // the side menu's switch follows
  }
  window.lowkeyTheme = function (c) {
    choice = c === "light" || c === "dark" ? c : "system";
    try {
      localStorage.setItem("lowkey-theme", choice);
    } catch (e) {}
    paint();
  };
  try {
    choice = localStorage.getItem("lowkey-theme") || "system";
  } catch (e) {}
  media.addEventListener("change", paint); // "System" follows the desktop while the app runs
  paint();
})();

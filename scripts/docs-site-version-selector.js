// Version selector for the documentation site that scripts/assemble-docs-site.py
// builds: this file sits at the root of one version's book, <site>/<version>/,
// and <site>/versions.json lists every version.
(() => {
  const script = document.currentScript;
  if (!script) return;
  const bookRoot = new URL("./", script.src);
  const siteRoot = new URL("../", bookRoot);
  const current = decodeURIComponent(bookRoot.pathname.slice(siteRoot.pathname.length, -1));
  const page = location.pathname.startsWith(bookRoot.pathname)
    ? location.pathname.slice(bookRoot.pathname.length)
    : "";

  const go = (version) => {
    const root = new URL(`${encodeURIComponent(version)}/`, siteRoot);
    const target = new URL(page, root);
    fetch(target, { method: "HEAD" })
      .then((response) => { location.href = response.ok ? target.href : root.href; })
      .catch(() => { location.href = root.href; });
  };

  const render = (index) => {
    const select = document.createElement("select");
    select.id = "version-selector";
    select.title = "Documentation version";
    select.setAttribute("aria-label", "Documentation version");
    for (const { version, prerelease } of index.versions) {
      const option = document.createElement("option");
      option.value = version;
      option.textContent = prerelease ? `${version} (pre-release)` : version;
      option.selected = version === current;
      select.append(option);
    }
    select.addEventListener("change", () => go(select.value));
    Object.assign(select.style, {
      font: "inherit",
      fontSize: "1.4rem",
      margin: "0 8px",
      padding: "2px 4px",
      verticalAlign: "middle",
      color: "var(--icons)",
      background: "var(--bg)",
      border: "1px solid var(--searchbar-border-color)",
      borderRadius: "4px",
      cursor: "pointer",
    });

    const buttons = document.querySelector(".menu-bar .right-buttons");
    if (buttons) {
      buttons.prepend(select);
    } else {
      Object.assign(select.style, { position: "fixed", top: "8px", right: "8px", zIndex: "100" });
      document.body.append(select);
    }
  };

  fetch(new URL("versions.json", siteRoot))
    .then((response) => (response.ok ? response.json() : Promise.reject(response.status)))
    .then(render)
    .catch(() => {});
})();

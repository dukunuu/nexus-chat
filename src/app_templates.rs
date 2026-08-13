//! Embedded starter scaffolds for the `app` tool's `init` action.
//!
//! Both templates are build-time-only: the model edits source, and `app
//! action=build` installs missing deps from package.json, then runs the
//! framework's static build. The served output (`dist/`) is pure static
//! files, so the appserver stays a static file server with no runtime
//! processes.
//!
//! Builds run with `--base=/<app-uuid>/` (passed by `app action=build`),
//! so asset links always resolve under the app's URL — no relative-base
//! tricks needed.

/// Astro + React islands. Static output, one interactive island demo.
const ASTRO_TEMPLATE: &[(&str, &str)] = &[
    (".gitignore", "node_modules/\ndist/\n"),
    (
        "package.json",
        r#"{
  "name": "app",
  "private": true,
  "type": "module",
  "scripts": {
    "build": "astro build",
    "dev": "astro dev"
  },
  "dependencies": {
    "@astrojs/react": "^4.2.0",
    "astro": "^5.5.0",
    "react": "^19.0.0",
    "react-dom": "^19.0.0"
  }
}
"#,
    ),
    (
        "astro.config.mjs",
        "// The app is served under /<uuid>/ — `app action=build` passes the\n\
// uuid base on the CLI, so leave `base` unset here.\n\
import { defineConfig } from 'astro/config';\n\
import react from '@astrojs/react';\n\
\n\
export default defineConfig({\n\
  output: 'static',\n\
  integrations: [react()],\n\
});\n",
    ),
    (
        "tsconfig.json",
        r#"{
  "extends": "astro/tsconfigs/base",
  "compilerOptions": {
    "jsx": "react-jsx",
    "jsxImportSource": "react"
  }
}
"#,
    ),
    (
        "src/pages/index.astro",
        "---\n\
// A page with a React island. Edit freely — nexus serves the built dist/.\n\
import Counter from '../components/Counter.tsx';\n\
const title = 'Nexus app';\n\
---\n\
<!doctype html>\n\
<html lang=\"en\">\n\
  <head>\n\
    <meta charset=\"utf-8\" />\n\
    <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n\
    <title>{title}</title>\n\
    <style>\n\
      :root { color-scheme: dark; }\n\
      body { font-family: system-ui, sans-serif; max-width: 40rem; margin: 3rem auto; padding: 0 1rem; line-height: 1.5; }\n\
      button { font-size: 1rem; padding: 0.5rem 1rem; border-radius: 8px; border: 1px solid #888; background: #222; color: #eee; cursor: pointer; }\n\
    </style>\n\
  </head>\n\
  <body>\n\
    <h1>{title}</h1>\n\
    <p>This page is built with Astro; the counter below is a React island.</p>\n\
    <Counter client:load />\n\
  </body>\n\
</html>\n",
    ),
    (
        "src/components/Counter.tsx",
        "// A React island. The app KV API (/_api/kv) persists state server-side.\n\
import { useEffect, useState } from 'react';\n\
\n\
// The app is served under /<uuid>/ — derive the uuid from the URL so KV\n\
// links work wherever the app is hosted.\n\
const base = `/${location.pathname.split('/')[1] || ''}`;\n\
\n\
export default function Counter() {\n\
  const [count, setCount] = useState(0);\n\
  const [saved, setSaved] = useState(false);\n\
\n\
  useEffect(() => {\n\
    fetch(`${base}/_api/kv/count`)\n\
      .then((r) => (r.ok ? r.text() : '0'))\n\
      .then((v) => setCount(Number(v) || 0))\n\
      .catch(() => {});\n\
  }, []);\n\
\n\
  const bump = async (delta: number) => {\n\
    const next = count + delta;\n\
    setCount(next);\n\
    setSaved(false);\n\
    try {\n\
      await fetch(`${base}/_api/kv/count`, {\n\
        method: 'PUT',\n\
        body: String(next),\n\
      });\n\
      setSaved(true);\n\
    } catch {\n\
      // KV is best-effort: the app still works without it.\n\
    }\n\
  };\n\
\n\
  return (\n\
    <div>\n\
      <p>\n\
        count: <strong>{count}</strong>{' '}\n\
        <button onClick={() => bump(1)}>+1</button>{' '}\n\
        <button onClick={() => bump(-1)}>-1</button>{' '}\n\
        {saved && <span>(saved)</span>}\n\
      </p>\n\
    </div>\n\
  );\n\
}\n",
    ),
    (
        "README.md",
        "# Astro + React app (scaffolded by the app tool)\n\
\n\
Workflow:\n\
\n\
1. Edit files with `app action=read|patch|write` (hash-line edits).\n\
2. Build: `app action=build` — installs missing deps from package.json,\n\
   compiles with `astro build`, and serves the result from `dist/`.\n\
3. Iterate: build errors come back in the tool result; fix and rebuild.\n\
\n\
Notes:\n\
\n\
- The app is served under `/<uuid>/`; `build` passes `--base=/<uuid>/`, so\n\
  asset links always resolve — don't hardcode absolute paths in source.\n\
- The KV API lives at `/<uuid>/_api/kv/<key>` (GET / PUT).\n\
- `node_modules/` and `dist/` are derived artifacts — never edit them by hand.\n",
    ),
];

/// Vite + React single-page app.
const VITE_TEMPLATE: &[(&str, &str)] = &[
    (".gitignore", "node_modules/\ndist/\n"),
    (
        "package.json",
        r#"{
  "name": "app",
  "private": true,
  "type": "module",
  "scripts": {
    "build": "vite build",
    "dev": "vite"
  },
  "dependencies": {
    "react": "^19.0.0",
    "react-dom": "^19.0.0"
  },
  "devDependencies": {
    "@types/react": "^19.0.0",
    "@types/react-dom": "^19.0.0",
    "@vitejs/plugin-react": "^4.4.0",
    "typescript": "^5.7.0",
    "vite": "^6.0.0"
  }
}
"#,
    ),
    (
        "tsconfig.json",
        r#"{
  "compilerOptions": {
    "target": "ES2022",
    "useDefineForClassFields": true,
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "skipLibCheck": true,
    "moduleResolution": "bundler",
    "allowImportingTsExtensions": true,
    "resolveJsonModule": true,
    "isolatedModules": true,
    "noEmit": true,
    "jsx": "react-jsx",
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true
  },
  "include": ["src"]
}
"#,
    ),
    (
        "vite.config.js",
        "// The app is served under /<uuid>/ — `app action=build` passes the\n\
// uuid base on the CLI, so leave `base` unset here.\n\
import { defineConfig } from 'vite';\n\
import react from '@vitejs/plugin-react';\n\
\n\
export default defineConfig({\n\
  plugins: [react()],\n\
});\n",
    ),
    (
        "index.html",
        "<!doctype html>\n\
<html lang=\"en\">\n\
  <head>\n\
    <meta charset=\"utf-8\" />\n\
    <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n\
    <title>Nexus app</title>\n\
  </head>\n\
  <body>\n\
    <div id=\"root\"></div>\n\
    <script type=\"module\" src=\"/src/main.tsx\"></script>\n\
  </body>\n\
</html>\n",
    ),
    (
        "src/main.tsx",
        "import { StrictMode } from 'react';\n\
import { createRoot } from 'react-dom/client';\n\
import App from './App.tsx';\n\
import './index.css';\n\
\n\
createRoot(document.getElementById('root')!).render(\n\
  <StrictMode>\n\
    <App />\n\
  </StrictMode>,\n\
);\n",
    ),
    (
        "src/App.tsx",
        "import { useEffect, useState } from 'react';\n\
\n\
// The app is served under /<uuid>/ — derive it so KV links work anywhere.\n\
const base = `/${location.pathname.split('/')[1] || ''}`;\n\
\n\
export default function App() {\n\
  const [count, setCount] = useState(0);\n\
  const [saved, setSaved] = useState(false);\n\
\n\
  useEffect(() => {\n\
    fetch(`${base}/_api/kv/count`)\n\
      .then((r) => (r.ok ? r.text() : '0'))\n\
      .then((v) => setCount(Number(v) || 0))\n\
      .catch(() => {});\n\
  }, []);\n\
\n\
  const bump = async (delta: number) => {\n\
    const next = count + delta;\n\
    setCount(next);\n\
    setSaved(false);\n\
    try {\n\
      await fetch(`${base}/_api/kv/count`, { method: 'PUT', body: String(next) });\n\
      setSaved(true);\n\
    } catch {\n\
      // KV is best-effort: the app still works without it.\n\
    }\n\
  };\n\
\n\
  return (\n\
    <main>\n\
      <h1>Nexus app (Vite + React + TypeScript)</h1>\n\
      <p>\n\
        count: <strong>{count}</strong>{' '}\n\
        <button onClick={() => bump(1)}>+1</button>{' '}\n\
        <button onClick={() => bump(-1)}>-1</button>{' '}\n\
        {saved && <span>(saved)</span>}\n\
      </p>\n\
    </main>\n\
  );\n\
}\n",
    ),
    (
        "src/index.css",
        ":root { color-scheme: dark; }\n\
body { font-family: system-ui, sans-serif; max-width: 40rem; margin: 3rem auto; padding: 0 1rem; line-height: 1.5; }\n\
button { font-size: 1rem; padding: 0.5rem 1rem; border-radius: 8px; border: 1px solid #888; background: #222; color: #eee; cursor: pointer; }\n",
    ),
    (
        "README.md",
        "# Vite + React + TypeScript app (scaffolded by the app tool)\n\
\n\
Workflow:\n\
\n\
1. Edit files with `app action=read|patch|write` (hash-line edits).\n\
2. Build: `app action=build` — installs missing deps from package.json,\n\
   compiles with `vite build`, and serves the result from `dist/`.\n\
3. Iterate: build errors come back in the tool result; fix and rebuild.\n\
\n\
Notes:\n\
\n\
- The app is served under `/<uuid>/`; `build` passes `--base=/<uuid>/`, so\n\
  asset links always resolve — don't hardcode absolute paths in source.\n\
- The KV API lives at `/<uuid>/_api/kv/<key>` (GET / PUT).\n\
- `node_modules/` and `dist/` are derived artifacts — never edit them by hand.\n",
    ),
];

/// Write a starter scaffold into `app_dir` for the given framework.
///
/// Refuses to overwrite anything — `init` only scaffolds fresh apps (the
/// model then edits with read/patch/write). Returns the written file paths.
pub fn scaffold(app_dir: &std::path::Path, framework: &str) -> Result<Vec<String>, String> {
    let files: &[(&str, &str)] = match framework {
        "astro" => ASTRO_TEMPLATE,
        "vite-react" => VITE_TEMPLATE,
        other => {
            return Err(format!(
                "unknown framework {other:?} — use \"astro\" or \"vite-react\""
            ));
        }
    };
    for (rel, _) in files {
        if app_dir.join(rel).exists() {
            return Err(format!(
                "{rel} already exists — init only scaffolds fresh apps (use patch/write to modify)"
            ));
        }
    }
    let mut written = Vec::with_capacity(files.len());
    for (rel, content) in files {
        let path = app_dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, content)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        written.push((*rel).to_string());
    }
    Ok(written)
}

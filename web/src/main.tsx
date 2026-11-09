import "@laputa-systems/ui-dashboard/styles.css";
import "./app.css";

import { render } from "@solidjs/web";
import { App } from "./app.tsx";

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");
render(() => <App />, root);

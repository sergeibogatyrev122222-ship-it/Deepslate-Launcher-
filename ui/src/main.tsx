import { render } from "solid-js/web";

import "./design/tokens.css";
import App from "./App";

const root = document.getElementById("root");
if (!root) {
  throw new Error("#root is missing from index.html");
}

render(() => <App />, root);

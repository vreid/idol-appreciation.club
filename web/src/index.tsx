import { render } from "solid-js/web";
import "chota/dist/chota.min.css";
import "./app.css";
import App from "./App";

const target = document.querySelector("#app");
if (target === null) {
  throw new Error("#app mount element is missing");
}

render(() => <App />, target);

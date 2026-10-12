import { mount } from "svelte";
import "./app.css";
import App from "./App.svelte";

const target = document.querySelector("#app");
if (target === null) {
  throw new Error("#app mount element is missing");
}

mount(App, { target });

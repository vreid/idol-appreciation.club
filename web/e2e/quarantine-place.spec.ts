import { expect, test } from "@playwright/test";
import { WORLD_CENTER } from "../src/lib/world";

// The 202 quarantine flow against a fully mocked API (docs
// scaling-architecture.md "Upload flow"): no live backend required, so
// this spec runs on either side of the pipeline flip and NEVER writes
// to the deployment BASE_URL points at. The poll fallback resolves the
// upload (the events stream here is an empty fulfill — the SSE lane
// stays silent, which is exactly the fallback's reason to exist).

// A 1×1 PNG (data URL) — the flow only needs a decodable image.
const PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

const UPLOAD_ID = "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a0c9";

function stubWorld(page: import("@playwright/test").Page): void {
  void page.route(
    "**/api/placements/near**",
    (route) =>
      void route.fulfill({
        status: 200,
        contentType: "application/json",
        body: "[]",
      }),
  );
  void page.route(
    "**/api/center",
    (route) =>
      void route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ current: null, all_time: [] }),
      }),
  );
  void page.route(
    "**/api/events",
    (route) =>
      void route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: "",
      }),
  );
  void page.route(
    "**/api/tiles/**",
    (route) =>
      void route.fulfill({
        status: 200,
        contentType: "image/webp",
        body: Buffer.alloc(1),
      }),
  );
  void page.route(
    "**/api/auth/me",
    (route) =>
      void route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          id: "u1",
          username: "bot",
          is_admin: false,
          cooldown_until: null,
        }),
      }),
  );
  void page.route(
    "**/api/versions**",
    (route) =>
      void route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ tiles: [] }),
      }),
  );
}

function stubPlace202(page: import("@playwright/test").Page): void {
  void page.route("**/api/place", (route) => {
    const body = route.request().postData() ?? "";
    const x = Number(/name="x"\r\n\r\n(-?\d+)/u.exec(body)?.[1] ?? "0");
    const y = Number(/name="y"\r\n\r\n(-?\d+)/u.exec(body)?.[1] ?? "0");
    void route.fulfill({
      status: 202,
      contentType: "application/json",
      body: JSON.stringify({
        id: UPLOAD_ID,
        state: "pending",
        x,
        y,
        w: 256,
        h: 256,
        rot: 0,
      }),
    });
  });
}

function screenPoint(
  page: import("@playwright/test").Page,
  dx: number,
  dy: number,
) {
  const vp = page.viewportSize() ?? { width: 1280, height: 720 };
  return { x: vp.width / 2 + dx, y: vp.height / 2 + dy };
}

async function placeViaChooser(
  page: import("@playwright/test").Page,
  chooserPromise: Promise<import("@playwright/test").FileChooser>,
): Promise<void> {
  const file = await chooserPromise;
  await file.setFiles({
    name: "pin.png",
    mimeType: "image/png",
    buffer: Buffer.from(PNG, "base64"),
  });
  const panel = page.locator(".place-panel");
  await expect(panel).toBeVisible();
  await panel.getByText(/Place at pinned spot/u).click();
  // The 202 acceptance closes the flow immediately (the pending
  // marker takes over the visual).
  await expect(panel).toHaveCount(0);
}

test("202 places park a pending marker and resolve placed via the poll", async ({
  page,
}) => {
  stubWorld(page);
  stubPlace202(page);
  let statusCalls = 0;
  void page.route(`**/api/uploads/${UPLOAD_ID}`, (route) => {
    statusCalls += 1;
    // One pending tick, then the worker's recording (the poll runs at
    // a 2 s cadence; the assert window is 5 s).
    const placed = statusCalls >= 2;
    void route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(
        placed
          ? {
              state: "placed",
              id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a0ff",
              seq: 5,
              bbox: [WORLD_CENTER, WORLD_CENTER, 256, 256],
            }
          : { state: "pending" },
      ),
    });
  });

  await page.goto("/#16/0/0");
  const target = screenPoint(page, 40, -40);
  const chooserPromise = page.waitForEvent("filechooser");
  await page.mouse.click(target.x, target.y, { button: "right" });
  await placeViaChooser(page, chooserPromise);

  // The user's own preview parks at the spot while the worker runs.
  await expect(page.locator(".pending-img")).toHaveCount(1);

  // The poll fallback resolves: success toast, marker released.
  await expect(page.getByText("Image placed!")).toBeVisible();
  await expect(page.locator(".pending-img")).toHaveCount(0);
});

test("a failed ingest toasts the reason and releases the marker", async ({
  page,
}) => {
  stubWorld(page);
  stubPlace202(page);
  void page.route(
    `**/api/uploads/${UPLOAD_ID}`,
    (route) =>
      void route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          state: "failed",
          attempts: 5,
          permanent: true,
          error: "decode failed",
        }),
      }),
  );

  await page.goto("/#16/0/0");
  const target = screenPoint(page, -60, 30);
  const chooserPromise = page.waitForEvent("filechooser");
  await page.mouse.click(target.x, target.y, { button: "right" });
  await placeViaChooser(page, chooserPromise);

  await expect(page.locator(".pending-img")).toHaveCount(1);

  // The failure carries the server's reason; no "still processing"
  // claim, no success.
  await expect(page.getByText("Placement failed: decode failed")).toBeVisible();
  await expect(page.locator(".pending-img")).toHaveCount(0);
  await expect(page.getByText("Image placed!")).toHaveCount(0);
});

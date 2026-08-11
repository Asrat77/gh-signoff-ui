const path = require("node:path");
const fs = require("node:fs");
const { defineConfig, devices } = require("@playwright/test");

const systemChrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const executablePath = process.env.PLAYWRIGHT_EXECUTABLE_PATH ||
  (process.platform === "darwin" && fs.existsSync(systemChrome) ? systemChrome : undefined);

module.exports = defineConfig({
  testDir: "./ui-tests",
  timeout: 15_000,
  expect: { timeout: 5_000 },
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? "github" : "list",
  use: {
    baseURL: "http://127.0.0.1:43199",
    ...devices["Desktop Chrome"],
    viewport: { width: 440, height: 700 },
    colorScheme: "light",
    reducedMotion: "reduce",
    launchOptions: executablePath ? { executablePath } : undefined,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: "go run . -demo -no-browser -port 43199",
    cwd: path.resolve(__dirname),
    url: "http://127.0.0.1:43199/",
    timeout: 120_000,
    reuseExistingServer: !process.env.CI,
  },
});

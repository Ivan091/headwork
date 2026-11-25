const event = JSON.parse(require("fs").readFileSync(process.env.GITHUB_EVENT_PATH, "utf8"));
const pull = event.pull_request || {};
process.stdout.write(process.argv[2] === "--number" ? String(pull.number || "") : pull.body || "");

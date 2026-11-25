const event = JSON.parse(require("fs").readFileSync(process.env.GITHUB_EVENT_PATH, "utf8"));
process.stdout.write((event.pull_request && event.pull_request.body) || "");

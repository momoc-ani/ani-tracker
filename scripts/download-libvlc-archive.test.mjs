import assert from "node:assert/strict";
import { readFile, rm, mkdtemp } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { downloadFile } from "./download-libvlc-archive.mjs";

test("下载器支持 HTTP 镜像重定向", async (context) => {
  const temporaryRoot = await mkdtemp(join(tmpdir(), "ani-libvlc-download-"));
  const destination = join(temporaryRoot, "vlc.zip");
  const server = createServer((request, response) => {
    if (request.url === "/archive") {
      response.writeHead(302, { location: "/mirror/vlc.zip" });
      response.end();
      return;
    }
    if (request.url === "/mirror/vlc.zip") {
      response.writeHead(200, { "content-type": "application/zip" });
      response.end("verified-vlc-archive");
      return;
    }
    response.writeHead(404);
    response.end();
  });

  await new Promise((resolveListen) => server.listen(0, "127.0.0.1", resolveListen));
  context.after(async () => {
    await new Promise((resolveClose, rejectClose) => {
      server.close((error) => error ? rejectClose(error) : resolveClose());
    });
    await rm(temporaryRoot, { recursive: true, force: true });
  });

  const address = server.address();
  assert(address && typeof address !== "string");
  await downloadFile(`http://127.0.0.1:${address.port}/archive`, destination, 5_000);
  assert.equal(await readFile(destination, "utf8"), "verified-vlc-archive");
});

test("下载器拒绝非 HTTP(S) 协议", async () => {
  await assert.rejects(
    downloadFile("file:///tmp/vlc.zip", "/tmp/unused-vlc.zip", 5_000),
    /Unsupported download protocol: file:/
  );
});

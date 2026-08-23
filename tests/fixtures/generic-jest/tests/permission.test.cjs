const { permission } = require("../src/permission.cjs");

test.concurrent("admin is allowed", async () => {
  await Promise.resolve();
  expect(permission(true, false)).toBe("allowed");
});

test.concurrent("owner is allowed", async () => {
  await Promise.resolve();
  expect(permission(false, true)).toBe("allowed");
});

test.concurrent.each([
  [true, true, "allowed"],
  [false, false, "denied"],
])("admin=%s owner=%s is %s", (admin, owner, expected) => {
  expect(permission(admin, owner)).toBe(expected);
});

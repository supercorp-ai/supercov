// The entry program executes unchanged, but the OS receives a different status.
process.on('exit', () => { process.exitCode = 1; });

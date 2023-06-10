import crypto from "crypto"

const SECRET = "MySecret"

const username = process.argv[2]
// const time = parseInt(Date.now() / 1000) + 24 * 3600
const time = parseInt(Date.now() / 1000) + 60 * 5
const user = `${time}:${username}`

const hmac = crypto.createHmac("sha1", SECRET)

hmac.setEncoding('base64')
hmac.write(user)
hmac.end()

const pass = hmac.read()
console.log({
  user, pass
})

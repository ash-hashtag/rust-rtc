import crypto from "crypto"

const SECRET = "MySecret"
const REALM = "earth"
const username = "user"
const ttl = 24 * 3600
const time = (parseInt(Date.now() / 1000) + ttl).toString()

getPass(time + ':' + REALM + ':' + username)
getPass(time)


function getPass(payload) {
  const hmac = crypto.createHmac("sha1", SECRET)

  hmac.setEncoding('base64')
  hmac.write(payload)
  hmac.end()

  const pass = hmac.read()
  console.log({
    payload, pass
  })
}


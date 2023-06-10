import {WebSocketServer} from "ws"

const server = new WebSocketServer({
  port: 8080,
})

server.on("connection", (socket, _request) => {
  console.log("new connection")

  socket.on("message", (data, _) => {
    const text = data.toString('utf8')
    console.log(text)
    server.clients.forEach(e => e != socket ? e.send(data) : null)
  })

  socket.on("close", (code, reason) => console.log(`Connection Closed ${code} ${reason}`))

})

import {WebSocketServer} from "ws"

const server = new WebSocketServer({
  host: '0.0.0.0',
  port: 9006,
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
